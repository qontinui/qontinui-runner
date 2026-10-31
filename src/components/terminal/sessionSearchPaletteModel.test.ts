import { beforeEach, describe, expect, it } from "vitest";

import { known } from "./sessionFacts";
import { MIN, NOW, facts } from "./sessionFacts.testkit";
import type { ClosedSession } from "./sessionOrgTypes";
import {
  buildPaletteEntries,
  paletteTerms,
  searchPalette,
  stepSelection,
  transcriptAnswerFor,
  transcriptCoverageNote,
} from "./sessionSearchPaletteModel";
import { requestSessionSearchOpen, sessionSearchPaletteStore } from "./sessionSearchPaletteStore";
import type { TranscriptSearchResponse } from "./transcriptSearch";
import type { PastSession } from "./usePastSessions";

const ID_A = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const ID_B = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
const ID_C = "cccccccc-cccc-cccc-cccc-cccccccccccc";

function past(id: string, name: string, over: Partial<PastSession> = {}): PastSession {
  return {
    claudeSessionId: id,
    resumeName: name,
    resumeCommand: `clg --resume ${id}`,
    account: { label: "gmail", wrapper: "clg" },
    pageId: "default",
    zoneIndex: 0,
    state: "closed",
    provider: "claude",
    lastSeenAt: NOW - 60 * MIN,
    openedAt: NOW - 120 * MIN,
    confirmed: true,
    transcriptExists: true,
    restorable: true,
    cohortId: 0,
    ...over,
  };
}

function closed(key: string, title: string): ClosedSession {
  return { sessionKey: key, title, closedAt: NOW - 5 * MIN, resumeCmd: null, groupId: null };
}

function transcript(over: Partial<TranscriptSearchResponse> = {}): TranscriptSearchResponse {
  return {
    terms: ["merge"],
    hits: [],
    matched: 0,
    scanned: 10,
    total: 10,
    failed: 0,
    truncated: false,
    truncation: [],
    notScanned: [],
    missing: [],
    elapsedMs: 3,
    ...over,
  };
}

describe("buildPaletteEntries", () => {
  it("lists a session once: live over previous over recently closed", () => {
    const entries = buildPaletteEntries({
      live: [facts({ rowId: "r1", claudeSessionId: ID_A, title: "Live A" })],
      previous: [past(ID_A, "Old A"), past(ID_B, "Old B")],
      closed: [closed(ID_B, "Closed B"), closed(ID_C, "Closed C"), closed("pane\u001fx", "Pane")],
    });
    expect(entries.map((e) => `${e.source}:${e.title}`)).toEqual([
      "live:Live A",
      "previous:Old B",
      "closed:Closed C",
      "closed:Pane",
    ]);
    const c = entries.find((e) => e.title === "Closed C");
    expect(c?.claudeSessionId).toBe(ID_C);
    expect(entries.find((e) => e.title === "Pane")?.claudeSessionId).toBeNull();
  });

  it("a live entry carries its row id; previous carries its resume command", () => {
    const [l, p] = buildPaletteEntries({
      live: [facts({ rowId: "r1", claudeSessionId: ID_A })],
      previous: [past(ID_B, "Old B")],
      closed: [],
    });
    expect(l.rowId).toBe("r1");
    expect(p.resumeCommand).toBe(`clg --resume ${ID_B}`);
  });
});

describe("searchPalette", () => {
  const entries = buildPaletteEntries({
    live: [
      facts({
        rowId: "r1",
        claudeSessionId: ID_A,
        title: "Runner fixes",
        repo: known("qontinui-runner"),
        lastActivityAt: known(NOW - 1 * MIN),
      }),
    ],
    previous: [past(ID_B, "Coord merge train", { workingDir: "/w/qontinui-coord" })],
    closed: [closed(ID_C, "Docs sweep")],
  });

  it("the empty query lists everything, live first", () => {
    expect(searchPalette(entries, "").map((m) => m.entry.source)).toEqual([
      "live",
      "previous",
      "closed",
    ]);
  });

  it("every word must match; a quoted phrase is one term", () => {
    expect(searchPalette(entries, "coord train").map((m) => m.entry.title)).toEqual([
      "Coord merge train",
    ]);
    expect(searchPalette(entries, '"coord merge"').length).toBe(1);
    expect(searchPalette(entries, '"merge coord"').length).toBe(0);
    expect(searchPalette(entries, "qontinui-runner").map((m) => m.entry.title)).toEqual([
      "Runner fixes",
    ]);
  });

  it("merges transcript hits with their snippet, after text matches", () => {
    const resp = transcript({
      hits: [
        {
          sessionId: ID_C,
          configDir: "/c",
          path: "/c/x.jsonl",
          hitCount: 2,
          snippet: "…the flaky test…",
          mtimeMs: 0,
        },
      ],
    });
    const m = searchPalette(entries, "flaky", resp);
    expect(m.map((x) => [x.entry.title, x.via, x.snippet])).toEqual([
      ["Docs sweep", "transcript", "…the flaky test…"],
    ]);
  });

  it("a field-looking term is searched as literal text", () => {
    expect(paletteTerms('repo:coord "a b" -x "open')).toEqual(["repo:coord", "a b", "-x", "open"]);
  });
});

describe("transcriptCoverageNote", () => {
  it("states coverage, and an incomplete scan is never presented as complete", () => {
    expect(transcriptCoverageNote(null, "idle")).toBeNull();
    expect(transcriptCoverageNote(null, "pending")).toBe("Searching transcripts…");
    expect(transcriptCoverageNote(null, "failed", "boom")).toBe("Transcript search failed: boom");
    expect(transcriptCoverageNote(transcript(), "done")).toBe("Transcripts: searched 10 of 10");
    const cut = transcriptCoverageNote(
      transcript({ scanned: 4, truncated: true, truncation: ["time_budget"] }),
      "done",
    );
    expect(cut).toContain("searched 4 of 10");
    expect(cut).toContain("incomplete (time_budget)");
  });
});

describe("stepSelection", () => {
  it("wraps, and is -1 on an empty list", () => {
    expect(stepSelection(0, -1, 3)).toBe(2);
    expect(stepSelection(2, 1, 3)).toBe(0);
    expect(stepSelection(0, 1, 0)).toBe(-1);
  });
});

describe("sessionSearchPaletteStore", () => {
  beforeEach(() => sessionSearchPaletteStore.setOpen(false));

  it("an open request with no palette mounted reports false and opens nothing", () => {
    expect(sessionSearchPaletteStore.mountedCount()).toBe(0);
    expect(requestSessionSearchOpen()).toBe(false);
    expect(sessionSearchPaletteStore.isOpen()).toBe(false);
  });

  it("opens while mounted, notifies, and closes when the last palette unmounts", () => {
    const unmount = sessionSearchPaletteStore.mount();
    let calls = 0;
    const unsub = sessionSearchPaletteStore.subscribe(() => calls++);
    expect(requestSessionSearchOpen()).toBe(true);
    expect(sessionSearchPaletteStore.isOpen()).toBe(true);
    sessionSearchPaletteStore.toggle();
    expect(sessionSearchPaletteStore.isOpen()).toBe(false);
    sessionSearchPaletteStore.toggle();
    unmount();
    expect(sessionSearchPaletteStore.isOpen()).toBe(false);
    expect(calls).toBe(4);
    unsub();
  });
});

describe("transcriptAnswerFor", () => {
  const resp = transcript({ terms: ["merge", "train"] });
  const kept = { query: "merge train", answer: { state: "done" as const, resp } };

  it("answers the query it was kept for", () => {
    expect(transcriptAnswerFor(kept, "merge train")).toBe(resp);
    expect(transcriptAnswerFor(kept, "  merge train ")).toBe(resp);
  });

  // Phase 8 live run: the `search` bridge action set a new query and filtered
  // in the same tick with the PREVIOUS query's response, so "closeme" listed
  // sessions whose transcripts only matched "merge train".
  it("is null for another query, so a stale answer never attributes hits", () => {
    expect(transcriptAnswerFor(kept, "closeme")).toBeNull();
  });

  it("is null when nothing is kept or the kept answer is not done", () => {
    expect(transcriptAnswerFor(null, "merge train")).toBeNull();
    expect(
      transcriptAnswerFor({ query: "merge train", answer: { state: "pending" } }, "merge train"),
    ).toBeNull();
  });
});
