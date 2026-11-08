import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, it, expect } from "vitest";
import {
  NON_RESTORABLE_CLOSE_REASONS,
  isUnfinished,
  parseResumeResponse,
  resumeUnfinished,
  unfinishedReadState,
} from "./unfinishedSessions";
import type { PastSession } from "./usePastSessions";

const s = (o: Partial<PastSession>) =>
  ({
    claudeSessionId: "a",
    state: "closed",
    provider: "claude",
    closedAt: 1,
    restorable: true,
    configDir: "/home/u/.claude",
    workingDir: "/w",
    ...o,
  }) as PastSession;

const noneLive = async () => new Set<string>();

describe("isUnfinished", () => {
  it("keeps closed, unfinished claude rows only", () => {
    expect(isUnfinished(s({}))).toBe(true);
    expect(isUnfinished(s({ finished: false }))).toBe(true);
    expect(isUnfinished(s({ finished: true }))).toBe(false);
    expect(isUnfinished(s({ state: "open" }))).toBe(false);
    expect(isUnfinished(s({ provider: "gemini" }))).toBe(false);
  });
  it("offers only what the resume door can act on", () => {
    // A snapshot row reports finished: false whatever the work's state.
    expect(isUnfinished(s({ closedAt: undefined }))).toBe(false);
    expect(isUnfinished(s({ restorable: false }))).toBe(false);
    expect(isUnfinished(s({ configDir: undefined }))).toBe(false);
    expect(isUnfinished(s({ workingDir: "  " }))).toBe(false);
    expect(isUnfinished(s({ closeReason: "terminal-only-idle" }))).toBe(false);
    expect(isUnfinished(s({ closeReason: "poll-dead" }))).toBe(true);
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
          {
            id: "b",
            outcome: "skipped",
            reason: "no-transcript",
            verdict: "skipped(no-transcript)",
          },
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
    const v = await resumeUnfinished(["a"], f, 9876, noneLive);
    expect(seen!.url).toBe("http://127.0.0.1:9876/control/sessions/resume");
    expect(JSON.parse(seen!.body)).toEqual({ ids: ["a"] });
    expect(v[0].verdict).toBe("failed(drained)");
  });
  it("reports a thrown fetch as unknown, because the door keeps working", async () => {
    const f = (async () => {
      throw new Error("down");
    }) as unknown as typeof fetch;
    const [v] = await resumeUnfinished(["a"], f, 1, noneLive);
    expect(v.outcome).toBe("unknown");
    expect(v.verdict).toBe("unknown(down)");
  });
  it("does not call the door for a session that is running", async () => {
    let calls = 0;
    const f = (async () => {
      calls += 1;
      return { ok: true, status: 200, json: async () => ({}) };
    }) as unknown as typeof fetch;
    const [v] = await resumeUnfinished(["a"], f, 1, async () => new Set(["a"]));
    expect(calls).toBe(0);
    expect(v.verdict).toBe("skipped(already-running)");
  });
  it("leaves the decision to the door when the live registry is unreadable", async () => {
    let calls = 0;
    const f = (async () => {
      calls += 1;
      return {
        ok: true,
        status: 200,
        json: async () => ({
          data: { results: [{ id: "a", outcome: "resumed", reason: null, verdict: "resumed" }] },
        }),
      };
    }) as unknown as typeof fetch;
    const [v] = await resumeUnfinished(["a"], f, 1, async () => null);
    expect(calls).toBe(1);
    expect(v.outcome).toBe("resumed");
  });
});

describe("NON_RESTORABLE_CLOSE_REASONS", () => {
  it("matches the Rust constant the runner's restore reads", () => {
    const src = readFileSync(
      fileURLToPath(
        new URL("../../../src-tauri/src/session/session_lifecycle_store.rs", import.meta.url),
      ),
      "utf8",
    );
    const m = /pub const NON_RESTORABLE_CLOSE_REASONS: &\[&str\] =\s*&\[([^\]]*)\]/.exec(src);
    if (!m) throw new Error("NON_RESTORABLE_CLOSE_REASONS not found in session_lifecycle_store.rs");
    const rust = [...m[1].matchAll(/"([^"]+)"/g)].map((x) => x[1]);
    expect(rust.length).toBeGreaterThan(0);
    expect([...NON_RESTORABLE_CLOSE_REASONS].sort()).toEqual([...rust].sort());
  });
});
