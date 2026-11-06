/**
 * Tests for the webview half of a handoff restart (`terminalHandoff.ts`).
 *
 * vitest runs `environment: "node"`, so the `listen` door is a recording fake
 * and the exit handler is exercised through `paneExitDisposition`, which is the
 * exact predicate `TerminalPage`'s `handleExit` branches on.
 */

import { beforeEach, describe, expect, it } from "vitest";
import {
  TERMINAL_HANDED_OFF_EVENT,
  isHandedOff,
  listenForHandoffs,
  noteHandedOff,
  paneExitDisposition,
  resetHandedOff,
  type TerminalHandedOffEvent,
} from "./terminalHandoff";

type Handler = (event: { payload: TerminalHandedOffEvent }) => void;

function fakeListen() {
  const handlers = new Map<string, Handler>();
  const listen = (event: string, handler: Handler) => {
    handlers.set(event, handler);
    return Promise.resolve(() => handlers.delete(event));
  };
  return { listen, handlers };
}

describe("terminal handoff", () => {
  beforeEach(() => resetHandedOff());

  it("subscribes to the runner's terminal-handed-off event", async () => {
    const { listen, handlers } = fakeListen();
    const unlisten = await listenForHandoffs(listen);
    expect([...handlers.keys()]).toEqual([TERMINAL_HANDED_OFF_EVENT]);
    expect(TERMINAL_HANDED_OFF_EVENT).toBe("terminal-handed-off");
    unlisten();
    expect(handlers.size).toBe(0);
  });

  it("a handed-off pane is not an exit; any other pane still is", async () => {
    const { listen, handlers } = fakeListen();
    await listenForHandoffs(listen);
    // Before the notice, an exit is an exit (non-vacuous baseline).
    expect(paneExitDisposition("term-holder")).toBe("exited");

    handlers.get(TERMINAL_HANDED_OFF_EVENT)!({
      payload: { terminalId: "term-holder", holderPid: 4242 },
    });

    expect(isHandedOff("term-holder")).toBe(true);
    expect(paneExitDisposition("term-holder")).toBe("handed-off");
    // A sibling in-process pane killed by the same teardown is still an exit.
    expect(paneExitDisposition("term-local")).toBe("exited");
  });

  it("ignores a payload without a terminal id", () => {
    noteHandedOff(undefined);
    noteHandedOff({ holderPid: 1 });
    noteHandedOff({ terminalId: "" });
    expect(paneExitDisposition("")).toBe("exited");
  });
});
