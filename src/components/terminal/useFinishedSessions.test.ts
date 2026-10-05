/**
 * Tests for the state handling behind the Terminal page's FINISHED band.
 *
 * The runner's vitest config is `environment: "node"` with no React Testing
 * Library (see `useCommitState.test.ts`), so the hook's pure halves — wire
 * parsing, the hold across unanswered reads, identity-preserving equality —
 * are asserted here, and its polling shell is not unit-tested.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, it, expect } from "vitest";
import {
  FINISHED_CHANGED_EVENT,
  holdFinished,
  parseFinishedStates,
  sameFinishedStates,
} from "./useFinishedSessions";

describe("parseFinishedStates", () => {
  it("reads the command's `{ sessions }` envelope", () => {
    expect(
      parseFinishedStates({
        sessions: {
          a: { verdict: "finished", source: "coord", finishedAt: 5 },
          b: { verdict: "not_finished" },
          c: { verdict: "unknown" },
        },
        coord: { degraded: false, note: "" },
      }),
    ).toEqual({
      a: { verdict: "finished", source: "coord", finishedAt: 5 },
      b: { verdict: "not_finished" },
      c: { verdict: "unknown" },
    });
  });

  it("drops an unrecognised verdict so it reads as UNKNOWN, not as either arm", () => {
    const out = parseFinishedStates({
      sessions: { a: { verdict: "done" }, b: null, c: { verdict: 1 } },
    });
    expect(out).toEqual({});
  });

  it("keeps the verdict but not a malformed source or timestamp", () => {
    expect(
      parseFinishedStates({
        sessions: { a: { verdict: "finished", source: "remote", finishedAt: "5" } },
      }),
    ).toEqual({ a: { verdict: "finished" } });
  });

  it("returns empty for anything that is not an envelope", () => {
    for (const data of [undefined, null, [], "x", { sessions: "x" }]) {
      expect(parseFinishedStates(data)).toEqual({});
    }
  });
});

describe("sameFinishedStates", () => {
  const a = { s1: { verdict: "finished" as const, source: "local" as const, finishedAt: 1 } };

  it("is true for structurally equal maps, so an idle poll keeps identity", () => {
    expect(sameFinishedStates(a, { s1: { ...a.s1 } })).toBe(true);
  });

  it("notices a verdict, source, timestamp or key change", () => {
    expect(sameFinishedStates(a, { s1: { ...a.s1, verdict: "unknown" } })).toBe(false);
    expect(sameFinishedStates(a, { s1: { ...a.s1, source: "both" } })).toBe(false);
    expect(sameFinishedStates(a, { s1: { ...a.s1, finishedAt: 2 } })).toBe(false);
    expect(sameFinishedStates(a, { s1: { ...a.s1, held: true } })).toBe(false);
    expect(sameFinishedStates(a, { s2: a.s1 })).toBe(false);
    expect(sameFinishedStates(a, {})).toBe(false);
  });
});

describe("holdFinished", () => {
  const ids = ["s1", "s2"];
  const finished = { verdict: "finished" as const, source: "coord" as const, finishedAt: 9 };
  const localOnly = { verdict: "finished" as const, source: "local" as const, finishedAt: 4 };

  it("does not hold a local-only mark the backend now answers unknown for", () => {
    // `unknown` = no local mark and coord unread: the local mark was removed.
    expect(holdFinished({ s1: localOnly }, { s1: { verdict: "unknown" } }, ids)).toEqual({
      s1: { verdict: "unknown" },
    });
    // A failed read says nothing about it, so it is still held.
    expect(holdFinished({ s1: localOnly }, {}, ids)).toEqual({
      s1: { verdict: "unknown", held: true, source: "local", finishedAt: 4 },
    });
  });

  it("holds a coord-backed finished session through a read that cannot answer", () => {
    // `unknown` from a degraded coord, and absent from a failed read.
    for (const next of [{ s1: { verdict: "unknown" as const } }, {}]) {
      expect(holdFinished({ s1: finished }, next, ids)).toEqual({
        s1: { verdict: "unknown", held: true, source: "coord", finishedAt: 9 },
      });
    }
  });

  it("keeps holding across consecutive unanswered reads", () => {
    const held = holdFinished({ s1: finished }, {}, ids);
    expect(holdFinished(held, {}, ids)).toEqual(held);
  });

  it("lets any answering read replace the hold", () => {
    const held = holdFinished({ s1: finished }, {}, ids);
    expect(holdFinished(held, { s1: { verdict: "not_finished" } }, ids)).toEqual({
      s1: { verdict: "not_finished" },
    });
    expect(holdFinished(held, { s1: finished }, ids)).toEqual({ s1: finished });
  });

  it("never invents a hold for a session that was not finished", () => {
    expect(
      holdFinished({ s1: { verdict: "not_finished" } }, { s1: { verdict: "unknown" } }, ids),
    ).toEqual({ s1: { verdict: "unknown" } });
  });

  it("keeps another page's finished sessions, held, so returning does not resize", () => {
    expect(holdFinished({ other: finished, idle: { verdict: "not_finished" } }, {}, ids)).toEqual({
      other: { verdict: "unknown", held: true, source: "coord", finishedAt: 9 },
    });
  });
});

describe("FINISHED_CHANGED_EVENT", () => {
  it("is the event name the runner emits", () => {
    const rust = readFileSync(
      fileURLToPath(
        new URL("../../../src-tauri/src/commands/terminal_finished.rs", import.meta.url),
      ),
      "utf8",
    );
    expect(rust).toContain(`pub const FINISHED_CHANGED_EVENT: &str = "${FINISHED_CHANGED_EVENT}";`);
  });
});
