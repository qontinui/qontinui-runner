import { describe, expect, it } from "vitest";

import { initialQueryDraft, queryDraftReducer, type QueryDraftEvent } from "./sessionQueryDraft";

const run = (query: string, events: QueryDraftEvent[]) =>
  events.reduce(queryDraftReducer, initialQueryDraft(query));
const typed = (value: string): QueryDraftEvent => ({ kind: "typed", value });
const prop = (query: string): QueryDraftEvent => ({ kind: "prop", query });

describe("queryDraftReducer", () => {
  // Phase 8 live run: bound straight to the manager's query, a UI Bridge
  // `type` into the box left the query unchanged (React restored the old
  // value and the driver's second onChange wrote it back). The box now owns
  // its text: an edit shows at once, whatever the manager has rendered.
  it("an edit shows at once, before the manager renders it back", () => {
    expect(run("", [typed("is:idle")]).draft).toBe("is:idle");
    expect(run("", [typed("is:idle"), prop("")]).draft).toBe("is:idle");
  });

  it("the manager's echo of the box's own edit changes nothing", () => {
    const s = run("", [typed("is:idle"), prop("is:idle")]);
    expect(s).toEqual({ draft: "is:idle", pending: [], seen: "is:idle" });
  });

  it("a late echo of an older edit does not roll the draft back", () => {
    const s = run("", [typed("i"), typed("is"), typed("is:"), prop("i")]);
    expect(s.draft).toBe("is:");
    expect(s.pending).toEqual(["is", "is:"]);
    // A transition may skip straight to the latest value.
    expect(queryDraftReducer(s, prop("is:"))).toEqual({ draft: "is:", pending: [], seen: "is:" });
  });

  it("a change from elsewhere (chip, picker, set-query, Clear) replaces the draft", () => {
    expect(run("", [prop("is:needs-you")]).draft).toBe("is:needs-you");
    const s = run("", [typed("repo:x"), prop("repo:x"), prop("")]);
    expect(s).toEqual({ draft: "", pending: [], seen: "" });
  });

  it("returns the same state when nothing changes", () => {
    const s = initialQueryDraft("is:idle");
    expect(queryDraftReducer(s, prop("is:idle"))).toBe(s);
    expect(queryDraftReducer(s, typed("is:idle"))).toBe(s);
  });
});
