import { describe, it, expect } from "vitest";
import { readFileSync } from "fs";
import { resolve } from "path";
import {
  MAX_RESUME_IDS_PER_CALL,
  isUnfinished,
  parseResumeResponse,
  resumableIds,
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

describe("resumableIds", () => {
  it("keeps only the rows with no blocker", () => {
    const rows = [s({ claudeSessionId: "a" }), s({ claudeSessionId: "b" })];
    expect(resumableIds(rows, (r) => (r.claudeSessionId === "b" ? "no" : null))).toEqual(["a"]);
  });
});

describe("parseResumeResponse", () => {
  it("maps per-id verdicts and fails unanswered ids", () => {
    // `ApiResponse<ResumeReport>` as `resume_door.rs` serializes it.
    const body = {
      success: true,
      data: {
        generation: null,
        generation_applied: false,
        requested: 3,
        resumed: 1,
        failed: 0,
        skipped: 1,
        results: [
          {
            id: "a",
            outcome: "resumed",
            reason: null,
            verdict: "resumed",
            evidence: {
              terminal_id: "t",
              pid: 42,
              cmdline: "claude --resume a",
              cwd: null,
              claude_config_dir: null,
            },
          },
          {
            id: "b",
            outcome: "skipped",
            reason: "no_transcript",
            verdict: "skipped(no_transcript)",
            evidence: null,
          },
        ],
      },
    };
    const v = parseResumeResponse(["a", "b", "c"], body);
    expect(v.map((x) => x.outcome)).toEqual(["resumed", "skipped", "failed"]);
    expect(v[1].verdict).toBe("skipped(no_transcript)");
    expect(v[2].reason).toBe("no-verdict-returned");
  });
  it("treats an unreadable body as failure for every id", () => {
    expect(parseResumeResponse(["a"], { success: false, error: "nope" })[0].reason).toBe("nope");
  });
});

describe("MAX_RESUME_IDS_PER_CALL", () => {
  it("matches the bound the door enforces", () => {
    const door = readFileSync(
      resolve(__dirname, "../../../src-tauri/src/session/resume_door.rs"),
      "utf8",
    );
    expect(door).toContain(`pub const MAX_IDS: usize = ${MAX_RESUME_IDS_PER_CALL};`);
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
  it("never sends the door more ids than it takes, and a failed call fails only its own", async () => {
    const calls: string[][] = [];
    const f = (async (_url: string, init: RequestInit) => {
      const { ids } = JSON.parse(String(init.body)) as { ids: string[] };
      calls.push(ids);
      if (calls.length === 2) {
        return { ok: false, status: 400, json: async () => ({ success: false, error: "bad" }) };
      }
      const results = ids.map((id) => ({
        id,
        outcome: "resumed",
        reason: null,
        verdict: "resumed",
        evidence: null,
      }));
      return { ok: true, status: 200, json: async () => ({ success: true, data: { results } }) };
    }) as unknown as typeof fetch;
    const ids = Array.from({ length: MAX_RESUME_IDS_PER_CALL + 1 }, (_, i) => `id-${i}`);
    const v = await resumeUnfinished(ids, f, 9876);
    expect(calls.map((c) => c.length)).toEqual([MAX_RESUME_IDS_PER_CALL, 1]);
    expect(v.map((x) => x.id)).toEqual(ids);
    expect(v[MAX_RESUME_IDS_PER_CALL - 1].outcome).toBe("resumed");
    expect(v[MAX_RESUME_IDS_PER_CALL].verdict).toBe("failed(bad)");
  });
  it("reports a thrown fetch as failure", async () => {
    const f = (async () => {
      throw new Error("down");
    }) as unknown as typeof fetch;
    expect((await resumeUnfinished(["a"], f, 1))[0].verdict).toBe("failed(down)");
  });
});
