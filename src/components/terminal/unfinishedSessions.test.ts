import { describe, it, expect } from "vitest";
import {
  isUnfinished,
  parseResumeResponse,
  resumeUnfinished,
  unfinishedReadState,
} from "./unfinishedSessions";
import type { PastSession } from "./usePastSessions";

const s = (o: Partial<PastSession>) =>
  ({ claudeSessionId: "a", state: "closed", provider: "claude", ...o }) as PastSession;

describe("isUnfinished", () => {
  it("keeps closed, unfinished claude rows only", () => {
    expect(isUnfinished(s({}))).toBe(true);
    expect(isUnfinished(s({ finished: false }))).toBe(true);
    expect(isUnfinished(s({ finished: true }))).toBe(false);
    expect(isUnfinished(s({ state: "open" }))).toBe(false);
    expect(isUnfinished(s({ provider: "gemini" }))).toBe(false);
  });
});

describe("unfinishedReadState", () => {
  it("keeps UNKNOWN distinct from empty", () => {
    expect(unfinishedReadState({ loaded: false, error: null, rows: [] })).toBe("unknown");
    expect(unfinishedReadState({ loaded: true, error: "boom", rows: [] })).toBe("unknown");
    expect(unfinishedReadState({ loaded: true, error: null, rows: [] })).toBe("empty");
    expect(unfinishedReadState({ loaded: true, error: null, rows: [1] })).toBe("rows");
  });
});

describe("parseResumeResponse", () => {
  it("maps per-id verdicts and fails unanswered ids", () => {
    const body = {
      success: true,
      data: {
        results: [
          { id: "a", outcome: "resumed", reason: null, verdict: "resumed" },
          { id: "b", outcome: "skipped", reason: "no-transcript", verdict: "skipped(no-transcript)" },
        ],
      },
    };
    const v = parseResumeResponse(["a", "b", "c"], body);
    expect(v.map((x) => x.outcome)).toEqual(["resumed", "skipped", "failed"]);
    expect(v[1].verdict).toBe("skipped(no-transcript)");
    expect(v[2].reason).toBe("no-verdict-returned");
  });
  it("treats an unreadable body as failure for every id", () => {
    expect(parseResumeResponse(["a"], { success: false, error: "nope" })[0].reason).toBe("nope");
  });
});

describe("resumeUnfinished", () => {
  it("posts ids to the door and reports http failure", async () => {
    let seen: { url: string; body: string } | null = null;
    const f = (async (url: string, init: RequestInit) => {
      seen = { url, body: String(init.body) };
      return { ok: false, status: 503, json: async () => ({ error: "drained" }) };
    }) as unknown as typeof fetch;
    const v = await resumeUnfinished(["a"], f, 9876);
    expect(seen!.url).toBe("http://127.0.0.1:9876/control/sessions/resume");
    expect(JSON.parse(seen!.body)).toEqual({ ids: ["a"] });
    expect(v[0].verdict).toBe("failed(drained)");
  });
  it("reports a thrown fetch as failure", async () => {
    const f = (async () => {
      throw new Error("down");
    }) as unknown as typeof fetch;
    expect((await resumeUnfinished(["a"], f, 1))[0].verdict).toBe("failed(down)");
  });
});
