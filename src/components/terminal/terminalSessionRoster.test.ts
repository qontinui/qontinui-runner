/**
 * Tests for the terminal-session roster payload (B4b).
 *
 * THE DEFECT: the roster emitted no liveness at all, so a tab whose PTY had
 * exited produced a row identical in every readable respect to a live one
 * (`isReconnecting: false` included). A verifier reading the roster to decide
 * "can I drive this terminal?" got a confident yes for a dead process.
 *
 * The fix surfaces the liveness the page ALREADY tracks under the same names
 * (`state` / `isAlive` / `exitCode`, as published in `TerminalSessionEntry`),
 * rather than introducing a second, disagreeable spelling.
 */

import { describe, expect, it } from "vitest";

import { buildTerminalSessionRoster } from "./terminalSessionRoster";

describe("buildTerminalSessionRoster", () => {
  it("reports an exited PTY with the page's own liveness fields", () => {
    const rows = buildTerminalSessionRoster(
      [{ id: "t-dead", title: "dead", isAlive: false, exitCode: 137 }],
      { 0: "t-dead" },
      { "t-dead": "error" },
    );
    expect(rows[0]).toEqual({
      claudeSessionId: null,
      terminalId: "t-dead",
      zoneIndex: 0,
      title: "dead",
      isReconnecting: false,
      state: "error",
      isAlive: false,
      exitCode: 137,
    });
  });

  it("uses the SAME field names as TerminalSessionEntry — no second spelling", () => {
    // A `dead` / `liveState` field here would be a fourth answer to tab
    // liveness in one component, which is the two-models defect one layer up.
    const [row] = buildTerminalSessionRoster(
      [{ id: "a", title: "A", isAlive: true }],
      {},
      {},
    );
    expect(Object.keys(row).sort()).toEqual([
      "claudeSessionId",
      "exitCode",
      "isAlive",
      "isReconnecting",
      "state",
      "terminalId",
      "title",
      "zoneIndex",
    ]);
  });

  it("defaults an untracked tab's state to idle, exactly like the sibling projection", () => {
    const rows = buildTerminalSessionRoster([{ id: "t-new", title: "new", isAlive: true }], {}, {});
    expect(rows[0].state).toBe("idle");
    expect(rows[0].isAlive).toBe(true);
    expect(rows[0].exitCode).toBeNull();
  });

  it("keeps a live session live and resolves its zone", () => {
    const rows = buildTerminalSessionRoster(
      [
        { id: "a", title: "A", isAlive: true, claudeSessionId: "sess-a" },
        { id: "b", title: "B", isAlive: false, exitCode: 0 },
      ],
      { 0: "b", 2: "a" },
      { a: "working", b: "completed" },
    );
    expect(rows[0]).toMatchObject({ zoneIndex: 2, isAlive: true, state: "working" });
    // Exit code 0 is still an exit — a clean finish is not a usable terminal.
    expect(rows[1]).toMatchObject({ zoneIndex: 0, isAlive: false, exitCode: 0 });
  });

  it("lists an unzoned session with zoneIndex -1 rather than dropping it", () => {
    const rows = buildTerminalSessionRoster([{ id: "ghost", title: "G" }], { 0: "other" }, {});
    expect(rows).toHaveLength(1);
    expect(rows[0].zoneIndex).toBe(-1);
  });

  it("survives the JSON round-trip the roster marker actually publishes", () => {
    const json = JSON.stringify(
      buildTerminalSessionRoster(
        [{ id: "t1", title: "T", isAlive: false, exitCode: 1 }],
        { 1: "t1" },
        { t1: "error" },
      ),
    );
    const parsed = JSON.parse(json) as Array<Record<string, unknown>>;
    expect(parsed[0].isAlive).toBe(false);
    expect(parsed[0].exitCode).toBe(1);
    expect(parsed[0].state).toBe("error");
  });
});
