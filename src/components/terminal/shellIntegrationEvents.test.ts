/**
 * `shellIntegrationEvents`: the OSC 633 parse a pane's `TerminalInstance` runs,
 * and the rule that a pane whose output this window stops parsing — a chunk
 * dropped while hidden, or an unmount — reports `unobserved`, so its page's
 * `useShellIntegration` forgets its prompt-state marks.
 *
 * `TerminalInstance` cannot be imported under `environment: "node"` (it pulls
 * `@xterm/addon-canvas`), so its wiring onto the sink is pinned by scanning its
 * source, the `paneOutputStream.test.ts` precedent.
 */

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

import { parseOsc633, shellMarksSink, type ShellIntegrationEvent } from "./shellIntegrationEvents";

describe("parseOsc633", () => {
  it("reads the prompt, command and capability marks", () => {
    expect(parseOsc633("A")).toEqual({ type: "prompt_start" });
    expect(parseOsc633("B")).toEqual({ type: "command_ready" });
    expect(parseOsc633("C")).toEqual({ type: "command_execute" });
    expect(parseOsc633("D;2")).toEqual({ type: "command_done", exitCode: 2 });
    expect(parseOsc633("E;echo a\\x3bb")).toEqual({ type: "command_line", command: "echo a;b" });
    expect(parseOsc633("P;Cwd=/repo")).toEqual({ type: "cwd", path: "/repo" });
    expect(parseOsc633("P;CommandMarks=1")).toEqual({ type: "command_marks" });
  });

  it("reads nothing it does not know", () => {
    expect(parseOsc633("P;CommandMarks=0")).toBeNull();
    expect(parseOsc633("Z")).toBeNull();
  });
});

describe("shellMarksSink", () => {
  function sink() {
    const events: ShellIntegrationEvent[] = [];
    let handler: ((e: ShellIntegrationEvent) => void) | undefined = (e) => events.push(e);
    const s = shellMarksSink(() => handler);
    return { s, events, detach: () => (handler = undefined) };
  }

  it("forwards each parsed OSC 633 sequence to the pane's current handler", () => {
    const { s, events } = sink();
    s.osc633("P;CommandMarks=1");
    s.osc633("A");
    s.osc633("Z");
    expect(events).toEqual([{ type: "command_marks" }, { type: "prompt_start" }]);
  });

  it("reports a chunk dropped unparsed as `unobserved`", () => {
    const { s, events } = sink();
    s.outputMissed();
    expect(events).toEqual([{ type: "unobserved" }]);
  });

  it("reports the pane's unmount as `unobserved`", () => {
    const { s, events } = sink();
    s.unmounted();
    expect(events).toEqual([{ type: "unobserved" }]);
  });

  it("drops events for a pane with no handler", () => {
    const { s, events, detach } = sink();
    detach();
    s.osc633("A");
    s.outputMissed();
    expect(events).toEqual([]);
  });
});

describe("TerminalInstance wiring onto the sink (source scan)", () => {
  const source = readFileSync(resolve(__dirname, "TerminalInstance.tsx"), "utf8");

  it("builds one sink per mount over the pane's live onShellIntegration", () => {
    expect(source).toContain("shellMarksSink(() => onShellIntegrationRef.current)");
  });

  it("feeds every OSC 633 sequence through the sink", () => {
    expect(source).toContain("backend.onOsc633((data) => shellMarks.osc633(data))");
  });

  it("reports a dropped chunk from the output stream's missed-output hook", () => {
    const hook = source.slice(source.indexOf("noteMissedOutput: (end) =>"));
    const body = hook.slice(0, hook.indexOf("requestResync:"));
    expect(body).toContain("paneService.noteMissedOutput(end)");
    expect(body).toContain("shellMarks.outputMissed()");
  });

  it("reports the unmount from the mount effect's cleanup", () => {
    const cleanup = source.slice(source.indexOf("window.removeEventListener(REMOTE_HISTORY_EVENT"));
    const body = cleanup.slice(0, cleanup.indexOf("paneService.dispose()"));
    expect(body).toContain("shellMarks.unmounted()");
  });
});
