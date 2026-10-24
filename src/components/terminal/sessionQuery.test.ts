import { describe, expect, it } from "vitest";

import { known, unknownFact, type PrFact, type SessionFacts } from "./sessionFacts";
import { HOUR, MIN, NOW, facts } from "./sessionFacts.testkit";
import {
  CONTAINS_FIELDS,
  IS_VALUES,
  WHERE_VALUES,
  collectTextQueries,
  evaluateClosable,
  filterSessions,
  findToken,
  hasToken,
  highlightQuery,
  parseDuration,
  parseQuery,
  setFieldValue,
  toggleToken,
  tokenizeQuery,
  type QueryContext,
  type TextSearchResult,
} from "./sessionQuery";
import { textSearchResultFrom, type TranscriptSearchResponse } from "./transcriptSearch";

const ctx: QueryContext = { now: NOW };

function match(query: string, f: SessionFacts, c: QueryContext = ctx): boolean | "unknown" {
  const r = filterSessions(query, [f], c);
  if (!r.ok) throw new Error(`parse failed: ${r.errors.map((e) => e.message).join("; ")}`);
  if (r.matched.length === 1) return true;
  if (r.unknown.length === 1) return "unknown";
  return false;
}

function pr(over: Partial<PrFact> = {}): PrFact {
  return {
    repo: "qontinui/qontinui-runner",
    number: 1,
    ci: "green",
    authorActs: false,
    reason: null,
    ...over,
  };
}

describe("tokenizeQuery", () => {
  it("tokenizes every kind with spans", () => {
    const q = 'foo "a b" -is:working (x OR y) idle>30m text:"merge, train"';
    const kinds = tokenizeQuery(q).map((t) => t.kind);
    expect(kinds).toEqual([
      "word",
      "phrase",
      "negate",
      "field",
      "lparen",
      "word",
      "or",
      "word",
      "rparen",
      "compare",
      "field",
    ]);
    for (const t of tokenizeQuery(q)) expect(q.slice(t.start, t.end).length).toBeGreaterThan(0);
  });

  it("keeps a quoted field value whole, punctuation included", () => {
    const [t] = tokenizeQuery('text:"merge, train!"');
    expect(t).toMatchObject({ kind: "field", name: "text", value: "merge, train!", quoted: true });
  });

  it("lowercase or is a word; a lone dash is a word", () => {
    expect(tokenizeQuery("a or b").map((t) => t.kind)).toEqual(["word", "word", "word"]);
    expect(tokenizeQuery("a - b").map((t) => t.kind)).toEqual(["word", "word", "word"]);
  });
});

describe("parseQuery", () => {
  it("empty and whitespace queries parse to null (= all)", () => {
    expect(parseQuery("")).toEqual({ ok: true, ast: null });
    expect(parseQuery("   ")).toEqual({ ok: true, ast: null });
  });

  it("AND binds tighter than OR", () => {
    const r = parseQuery("a b OR c");
    expect(r.ok && r.ast?.kind).toBe("or");
    if (r.ok && r.ast?.kind === "or") {
      expect(r.ast.children[0].kind).toBe("and");
      expect(r.ast.children[1]).toMatchObject({ kind: "text", value: "c" });
    }
  });

  it("parentheses override precedence", () => {
    const r = parseQuery("a (b OR c)");
    expect(r.ok && r.ast?.kind).toBe("and");
  });

  it.each([
    ["foo:bar", "unknown-field"],
    ["http://example.com", "unknown-field"],
    ["is:sleeping", "unknown-value"],
    ["where:mars", "unknown-value"],
    ["color:red", "unknown-value"],
    ["is:", "missing-value"],
    ["pr:abc", "invalid-number"],
    ["msgs>lots", "invalid-number"],
    ["idle>30", "invalid-duration"],
    ["age<soon", "invalid-duration"],
    ["started:tomorrow", "invalid-date"],
    ["speed>3m", "unknown-field"],
    ['"open phrase', "unterminated-quote"],
    ['text:"open', "unterminated-quote"],
    ["(a b", "unbalanced-paren"],
    ["a b)", "unbalanced-paren"],
    ["a OR", "dangling-or"],
    ["OR a", "dangling-or"],
    ["a -", "word-not-error"],
    ["a -(", "unbalanced-paren"],
  ])("%s → %s", (q, code) => {
    const r = parseQuery(q);
    if (code === "word-not-error") {
      expect(r.ok).toBe(true);
      return;
    }
    expect(r.ok).toBe(false);
    if (!r.ok) {
      expect(r.errors.map((e) => e.code)).toContain(code);
      for (const e of r.errors) expect(e.message.length).toBeGreaterThan(0);
    }
  });

  it("a dangling negation before OR is an error", () => {
    const r = parseQuery("a -OR b");
    // `-` directly before a non-space is a negation, here of nothing.
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.errors[0].code).toBe("dangling-negation");
  });

  it("an error carries the span of the offending term", () => {
    const r = parseQuery("is:working foo:bar");
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.errors[0]).toMatchObject({ start: 11, end: 18 });
  });

  it("a query with any error evaluates nothing", () => {
    const r = filterSessions("is:working bogus:1", [facts()], ctx);
    expect(r.ok).toBe(false);
  });

  it("accepts every documented field and value", () => {
    for (const v of IS_VALUES) expect(parseQuery(`is:${v}`).ok).toBe(true);
    for (const v of WHERE_VALUES) expect(parseQuery(`where:${v}`).ok).toBe(true);
    for (const f of CONTAINS_FIELDS) expect(parseQuery(`${f}:x`).ok).toBe(true);
    for (const q of [
      "title:x",
      "name:x",
      'text:"x y"',
      "color:info",
      "color:none",
      "tag:x",
      "pr:12",
      "pr:#12",
      "started:today",
      "started:yesterday",
      "started:2026-10-13",
      "idle>30m",
      "age<=2h",
      "msgs>=100",
      "msgs=3",
    ]) {
      expect(parseQuery(q).ok, q).toBe(true);
    }
  });
});

describe("parseDuration", () => {
  it("reads every unit", () => {
    expect(parseDuration("45s")).toBe(45_000);
    expect(parseDuration("30m")).toBe(30 * MIN);
    expect(parseDuration("2h")).toBe(2 * HOUR);
    expect(parseDuration("1d")).toBe(24 * HOUR);
    expect(parseDuration("1w")).toBe(7 * 24 * HOUR);
    expect(parseDuration("1.5h")).toBe(1.5 * HOUR);
    expect(parseDuration("30")).toBeNull();
  });
});

describe("free text", () => {
  it("bare words AND over title, topic, preview and names, case-insensitively", () => {
    expect(match("runner MERGE", facts())).toBe(true);
    expect(match("hello", facts())).toBe(true);
    expect(match("qontinui-runner", facts())).toBe(true);
    expect(match("runner absent", facts())).toBe(false);
  });

  it("a quoted phrase must appear verbatim, spanning punctuation", () => {
    const f = facts({ topic: "Task: fix the merge-train, then ship" });
    expect(match('"merge-train, then"', f)).toBe(true);
    expect(match('"train merge"', f)).toBe(false);
  });

  it("title: and name: match the title only", () => {
    expect(match("title:fixes", facts())).toBe(true);
    expect(match("name:fixes", facts())).toBe(true);
    // In the topic but not the title.
    expect(match("title:merge", facts())).toBe(false);
    expect(match('title:"Runner fixes"', facts())).toBe(true);
  });
});

describe("is:", () => {
  it.each([
    ["needs-you", "needs-you", true],
    ["working", "working", true],
    ["idle", "idle-at-prompt", true],
    ["stalled", "no-output-mid-tool", true],
    ["exited", "exited", true],
    ["exited", "exited-error", true],
    ["error", "exited-error", true],
    ["error", "exited", false],
    ["working", "idle-at-prompt", false],
  ] as const)("is:%s over activity %s → %s", (value, activity, want) => {
    expect(match(`is:${value}`, facts({ activity: known(activity) }))).toBe(want);
  });

  it("activity-backed values are unknown when the activity is", () => {
    expect(match("is:working", facts({ activity: unknownFact("no digest") }))).toBe("unknown");
  });

  it("finished, pinned, muted read their facts", () => {
    expect(match("is:finished", facts({ finished: known(true) }))).toBe(true);
    expect(match("is:finished", facts({ finished: unknownFact("x") }))).toBe("unknown");
    expect(match("is:pinned", facts({ pinned: known(true) }))).toBe(true);
    expect(match("is:pinned", facts())).toBe(false);
    expect(match("is:muted", facts({ muted: known(true) }))).toBe(true);
    expect(match("is:muted", facts({ muted: unknownFact("org loading") }))).toBe("unknown");
  });

  it("snoozed means snoozed until a time still ahead", () => {
    expect(match("is:snoozed", facts({ snoozedUntil: known(NOW + MIN) }))).toBe(true);
    expect(match("is:snoozed", facts({ snoozedUntil: known(NOW - MIN) }))).toBe(false);
    expect(match("is:snoozed", facts())).toBe(false);
  });

  it("unseen reads the card dot's verdict, Unknown included", () => {
    expect(match("is:unseen", facts({ unseen: known(true) }))).toBe(true);
    expect(match("is:unseen", facts({ unseen: known(false) }))).toBe(false);
    expect(match("is:unseen", facts({ unseen: unknownFact("org") }))).toBe("unknown");
    // Not re-derived from lastSeenAt: the dot's verdict alone decides.
    expect(match("is:unseen", facts({ lastSeenAt: known(null), unseen: known(false) }))).toBe(
      false,
    );
  });
});

describe("is:closable", () => {
  const exited0 = { activity: known("exited" as const), exitCode: 0, finished: known(false) };

  it("the finished marker alone is closable", () => {
    expect(evaluateClosable(facts({ finished: known(true) })).value).toBe(true);
  });

  it("exited 0 with every PR green, landed or closed is closable", () => {
    const f = facts({
      ...exited0,
      prs: known([pr(), pr({ number: 2, ci: "landed" }), pr({ number: 3, ci: "closed" })]),
    });
    expect(match("is:closable", f)).toBe(true);
  });

  it("exited 0 with no PRs (and a scanned ledger) is closable", () => {
    expect(match("is:closable", facts({ ...exited0, prs: known([]) }))).toBe(true);
  });

  it("author_acts is owed work: not closable, even when finished", () => {
    const owed = known([pr({ authorActs: true })]);
    expect(match("is:closable", facts({ ...exited0, prs: owed }))).toBe(false);
    expect(match("is:closable", facts({ finished: known(true), prs: owed }))).toBe(false);
  });

  it("a PR with no CI source is unknown, never closable", () => {
    const f = facts({
      ...exited0,
      prs: known([pr({ ci: "unknown", reason: "coord unreachable" })]),
    });
    const r = filterSessions("is:closable", [f], ctx);
    expect(r.ok && r.unknown.length).toBe(1);
    if (r.ok) expect(r.unknown[0].reason).toContain("coord unreachable");
  });

  it("a red PR is unknown (attribution not established); a pending one is not closable", () => {
    expect(match("is:closable", facts({ ...exited0, prs: known([pr({ ci: "red" })]) }))).toBe(
      "unknown",
    );
    expect(match("is:closable", facts({ ...exited0, prs: known([pr({ ci: "pending" })]) }))).toBe(
      false,
    );
  });

  it("an unknown PR ledger after a clean exit is unknown", () => {
    expect(match("is:closable", facts({ ...exited0, prs: unknownFact("ledger down") }))).toBe(
      "unknown",
    );
  });

  it("a live, unfinished session is not closable", () => {
    expect(match("is:closable", facts())).toBe(false);
  });

  it("a non-zero exit is not closable; an unknown exit code is unknown", () => {
    expect(
      match("is:closable", facts({ ...exited0, activity: known("exited-error"), exitCode: 2 })),
    ).toBe(false);
    expect(match("is:closable", facts({ ...exited0, exitCode: null }))).toBe("unknown");
  });

  it("live with an unknown finished marker is unknown, not 'not closable'", () => {
    expect(match("is:closable", facts({ finished: unknownFact("no info") }))).toBe("unknown");
  });
});

describe("where:", () => {
  it.each([
    ["screen", "on-screen"],
    ["hidden", "hidden-here"],
    ["outside", "outside-runner"],
    ["device", "other-device"],
    ["not-running", "not-running"],
  ] as const)("where:%s matches %s", (value, location) => {
    expect(match(`where:${value}`, facts({ location: known(location) }))).toBe(true);
    expect(match(`where:${value}`, facts({ location: known("on-screen") }))).toBe(
      value === "screen",
    );
  });

  it("an unknown location is unknown, never not-running", () => {
    expect(match("where:not-running", facts({ location: unknownFact("scan degraded") }))).toBe(
      "unknown",
    );
  });
});

describe("organization and identity fields", () => {
  it("group: substring of the title or exact id; none = ungrouped", () => {
    expect(match("group:coord", facts())).toBe(true);
    expect(match("group:g1", facts())).toBe(true);
    expect(match("group:none", facts())).toBe(false);
    expect(match("group:none", facts({ group: known(null) }))).toBe(true);
    expect(match('group:"coord work"', facts())).toBe(true);
    expect(match("group:x", facts({ group: unknownFact("org") }))).toBe("unknown");
  });

  it("color: is the exact role; none = ungrouped", () => {
    expect(match("color:info", facts())).toBe(true);
    expect(match("color:brand-primary", facts())).toBe(false);
    expect(match("color:none", facts({ group: known(null) }))).toBe(true);
  });

  it("tag: is one exact tag, case-insensitively", () => {
    expect(match("tag:urgent", facts())).toBe(true);
    expect(match("tag:urg", facts())).toBe(false);
  });

  it.each([
    ["account:pakt", true],
    ["account:other", false],
    ["repo:runner", true],
    ["repo:coord", false],
    ["branch:feature", true],
    ["plan:session-manager", true],
    ["unit:2026-10-13", true],
    ["device:space", true],
    ["page:main", true],
    ["page:p1", true],
    ["page:other", false],
  ] as const)("%s → %s", (q, want) => {
    expect(match(q, facts())).toBe(want);
  });

  it("a null value never matches a substring; an unknown one is unknown", () => {
    expect(match("branch:x", facts({ branch: known(null) }))).toBe(false);
    expect(match("page:main", facts({ page: known(null) }))).toBe(false);
    expect(match("repo:x", facts({ repo: unknownFact("no dir") }))).toBe("unknown");
    expect(match("plan:x", facts({ workUnit: unknownFact("no fleet row") }))).toBe("unknown");
  });

  it("pr: matches an authored PR number", () => {
    const f = facts({ prs: known([pr({ number: 42 })]) });
    expect(match("pr:42", f)).toBe(true);
    expect(match("pr:#42", f)).toBe(true);
    expect(match("pr:43", f)).toBe(false);
    expect(match("pr:42", facts({ prs: unknownFact("ledger") }))).toBe("unknown");
  });
});

describe("comparisons and started:", () => {
  it("idle compares time since the last activity", () => {
    expect(match("idle>5m", facts())).toBe(true);
    expect(match("idle>30m", facts())).toBe(false);
    expect(match("idle<=10m", facts())).toBe(true);
    expect(match("idle>1m", facts({ lastActivityAt: unknownFact("none") }))).toBe("unknown");
  });

  it("age compares time since start; msgs compares the count", () => {
    expect(match("age>2h", facts())).toBe(true);
    expect(match("age<2h", facts())).toBe(false);
    expect(match("msgs>100", facts())).toBe(true);
    expect(match("msgs=120", facts())).toBe(true);
    expect(match("msgs<100", facts())).toBe(false);
    expect(match("msgs>1", facts({ messageCount: unknownFact("tab") }))).toBe("unknown");
  });

  it("started: is a local calendar day", () => {
    expect(match("started:today", facts())).toBe(true);
    expect(match("started:yesterday", facts())).toBe(false);
    expect(match("started:yesterday", facts({ startedAt: known(NOW - 24 * HOUR) }))).toBe(true);
    expect(match("started:2026-10-13", facts())).toBe(true);
    expect(match("started:2026-10-12", facts())).toBe(false);
  });
});

describe("text:", () => {
  const done = (
    over: Partial<Extract<TextSearchResult, { status: "done" }>> = {},
  ): TextSearchResult => ({
    status: "done",
    matched: new Set(["sid"]),
    notScanned: new Set(),
    missing: new Set(),
    limitHit: false,
    incomplete: false,
    ...over,
  });
  const withText = (q: string, r: TextSearchResult): QueryContext => ({
    now: NOW,
    textSearch: new Map([[q, r]]),
  });

  it("collects each text: query, quoted ones as phrases", () => {
    const r = parseQuery('text:"merge train" OR -text:coord');
    expect(r.ok && collectTextQueries(r.ast)).toEqual(['"merge train"', "coord"]);
  });

  it("a matched session is true; a scanned non-match is false", () => {
    expect(match('text:"merge train"', facts(), withText('"merge train"', done()))).toBe(true);
    expect(
      match('text:"merge train"', facts(), withText('"merge train"', done({ matched: new Set() }))),
    ).toBe(false);
  });

  it("pending, failed, not-scanned, limit-hit and incomplete answers are unknown", () => {
    const q = "coord";
    const none = new Set<string>();
    for (const r of [
      { status: "pending" } as TextSearchResult,
      { status: "failed", reason: "ipc" } as TextSearchResult,
      done({ matched: none, notScanned: new Set(["sid"]) }),
      done({ matched: none, limitHit: true }),
      done({ matched: none, incomplete: true }),
    ]) {
      expect(match(`text:${q}`, facts(), withText(q, r))).toBe("unknown");
    }
    // No result at all for the query: unknown too.
    expect(match("text:coord", facts())).toBe("unknown");
  });

  describe("through textSearchResultFrom (the backend response the hook receives)", () => {
    const resp = (over: Partial<TranscriptSearchResponse> = {}): TranscriptSearchResponse => ({
      terms: ["coord"],
      hits: [],
      matched: 0,
      scanned: 1,
      total: 1,
      failed: 0,
      truncated: false,
      truncation: [],
      notScanned: [],
      missing: [],
      elapsedMs: 1,
      ...over,
    });
    const viaHook = (r: TranscriptSearchResponse) =>
      match("text:coord", facts(), withText("coord", textSearchResultFrom(r, true)));

    it("a complete scoped scan with no hit is false", () => {
      expect(viaHook(resp())).toBe(false);
    });

    it("a scoped scan that never ran (not_run) is unknown even with an empty notScanned", () => {
      expect(
        viaHook(resp({ scanned: 0, total: 0, truncated: true, truncation: ["not_run"] })),
      ).toBe("unknown");
    });

    it("a scoped scan that read fewer files than it had, naming none, is unknown", () => {
      expect(viaHook(resp({ scanned: 1, total: 2 }))).toBe("unknown");
    });

    it("a named session with no transcript found (missing) is unknown, not no-match", () => {
      const f = filterSessions(
        "text:coord",
        [facts()],
        withText("coord", textSearchResultFrom(resp({ scanned: 0, total: 0, missing: ["sid"] }), true)),
      );
      expect(f.ok && f.unknown.map((u) => u.reason)).toEqual([
        "no transcript found on this runner",
      ]);
    });
  });

  it("a row with no Claude session id is unknown for text:", () => {
    expect(match("text:x", facts({ claudeSessionId: null }), withText("x", done()))).toBe(
      "unknown",
    );
  });
});

describe("three-valued logic", () => {
  const unk = facts({ activity: unknownFact("no digest") });

  it("negation of unknown stays unknown", () => {
    expect(match("-is:working", unk)).toBe("unknown");
    expect(match("-is:working", facts())).toBe(false);
    expect(match("-is:idle", facts())).toBe(true);
  });

  it("AND: false dominates unknown; OR: true dominates unknown", () => {
    expect(match("is:working absentword", unk)).toBe(false);
    expect(match("is:working runner", unk)).toBe("unknown");
    expect(match("is:working OR runner", unk)).toBe(true);
    expect(match("is:working OR absentword", unk)).toBe("unknown");
    expect(match("absentword OR otherabsent", unk)).toBe(false);
  });

  it("empty query matches every row, unknown facts included", () => {
    const r = filterSessions("", [facts(), unk], ctx);
    expect(r.ok && r.matched.length).toBe(2);
    expect(r.ok && r.unknown.length).toBe(0);
  });

  it("partitions rows into matched, unknown (with reason) and excluded", () => {
    const rows = [
      facts({ rowId: "a" }),
      facts({ rowId: "b", activity: known("idle-at-prompt") }),
      facts({ rowId: "c", activity: unknownFact("no transcript digest") }),
    ];
    const r = filterSessions("is:working", rows, ctx);
    expect(r.ok).toBe(true);
    if (!r.ok) return;
    expect(r.matched.map((f) => f.rowId)).toEqual(["a"]);
    // The mutation check on the Unknown tail: if unknown were folded into
    // excluded (or matched), this partition reddens.
    expect(r.unknown.map((u) => u.facts.rowId)).toEqual(["c"]);
    expect(r.unknown[0].reason).toBe("no transcript digest");
    expect(r.excluded).toBe(1);
  });
});

describe("chips edit the query", () => {
  it("toggle appends then removes a token", () => {
    expect(toggleToken("", "is:needs-you")).toBe("is:needs-you");
    expect(toggleToken("repo:coord", "is:needs-you")).toBe("repo:coord is:needs-you");
    expect(toggleToken("repo:coord is:needs-you", "is:needs-you")).toBe("repo:coord");
    expect(toggleToken("is:needs-you repo:coord", "IS:NEEDS-YOU")).toBe("repo:coord");
    expect(toggleToken("a is:pinned b", "is:pinned")).toBe("a b");
  });

  it("a negated or parenthesized token does not light the chip", () => {
    expect(hasToken("-is:pinned", "is:pinned")).toBe(false);
    expect(hasToken("(is:pinned OR is:muted)", "is:pinned")).toBe(false);
    expect(hasToken("x is:pinned", "is:pinned")).toBe(true);
    expect(findToken("x is:pinned", "is:pinned")).toEqual({ start: 2, end: 11 });
  });

  it("toggling onto a top-level OR groups the OR first, preserving its meaning", () => {
    expect(hasToken("is:working OR is:idle", "is:working")).toBe(false);
    expect(toggleToken("is:working OR is:idle", "repo:x")).toBe("(is:working OR is:idle) repo:x");
  });

  it("setFieldValue replaces a picker's single value", () => {
    expect(setFieldValue("is:working account:a", "account", "b")).toBe("is:working account:b");
    expect(setFieldValue("is:working account:a", "account", null)).toBe("is:working");
    expect(setFieldValue("", "group", "Coord work")).toBe('group:"Coord work"');
    // A negated term is the operator's, not the picker's.
    expect(setFieldValue("-account:a", "account", "b")).toBe("-account:a account:b");
  });

  it("the chip's token and typed text agree", () => {
    const q = toggleToken("", "is:closable");
    expect(parseQuery(q).ok).toBe(true);
    expect(hasToken(q, "is:closable")).toBe(true);
  });
});

describe("highlightQuery", () => {
  it("covers the whole string and classifies each part", () => {
    const q = 'foo -is:working OR "a b" (x)';
    const segs = highlightQuery(q);
    expect(segs.map((s) => s.text).join("")).toBe(q);
    const kinds = segs.filter((s) => s.kind !== "space").map((s) => [s.text, s.kind]);
    expect(kinds).toEqual([
      ["foo", "word"],
      ["-", "negate"],
      ["is:", "field-name"],
      ["working", "field-value"],
      ["OR", "operator"],
      ['"a b"', "phrase"],
      ["(", "paren"],
      ["x", "word"],
      [")", "paren"],
    ]);
  });

  it("marks the erroneous term", () => {
    const segs = highlightQuery("is:working nope:1");
    expect(segs.find((s) => s.kind === "error")?.text).toBe("nope:1");
    expect(segs.find((s) => s.text === "is:")?.kind).toBe("field-name");
  });

  it("splits a comparison into name and value", () => {
    const segs = highlightQuery("idle>30m");
    expect(segs.map((s) => [s.text, s.kind])).toEqual([
      ["idle>", "field-name"],
      ["30m", "field-value"],
    ]);
  });
});
