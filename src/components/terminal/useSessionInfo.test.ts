/**
 * Tests for the pure helpers behind the zone-header session-info dropdown.
 *
 * The hook itself needs a DOM + Tauri IPC; the runner's vitest config uses
 * `environment: "node"` (no jsdom — see `CommitTrafficLight.test.tsx`,
 * `LaunchMenu.test.tsx` for the precedent), so every decision that drives a
 * rendered value lives in a pure function and is asserted here. The JSX shell
 * is verified through the UI Bridge (P6), which is why the element ids and
 * the field/raw-value contract are themselves unit-tested below.
 *
 * Migrated from the deleted test file of the PR-only hook this module subsumes
 * (plan §5 P4), with the vocabulary corrected from "merged" to "landed" —
 * coord fast-forward lands leave `merged=false`, which is the G3 defect:
 *   - `sortPrsUnmergedFirst`  → `sortPrsUnlandedFirst` (ordering + no-mutation)
 *   - `summarizePrs`          → same name, three buckets instead of one flag
 *   - `prGithubUrl` / `prLabel` → unchanged, INCLUDING the load-bearing
 *     "refuses a link for a bare checkout-name repo" case.
 *
 * New coverage for this plan: the summary string (with the unknown bucket),
 * the opened/landed/unknown split, and the unknown-state rendering path that
 * used to be a `return null` (G5).
 */

import { describe, it, expect } from "vitest";
import {
  deriveTrigger,
  formatEpochMs,
  normalizeSessionInfo,
  prGithubUrl,
  prKey,
  prLabel,
  prPanelRows,
  prRowChip,
  sessionInfoElementId,
  sessionInfoRows,
  sortPrsUnlandedFirst,
  summarizePrs,
  TONE_COLORS,
  UNKNOWN_TEXT,
  type SessionInfoBody,
  type SessionInfoState,
  type SessionPrOpened,
  type SessionPrs,
} from "./useSessionInfo";

function opened(
  overrides: Partial<SessionPrOpened> & Pick<SessionPrOpened, "prNumber">,
): SessionPrOpened {
  return {
    repo: "qontinui/qontinui-runner",
    branch: "feat/x",
    openedAt: null,
    prState: "open",
    ...overrides,
  };
}

function prs(overrides: Partial<SessionPrs> = {}): SessionPrs {
  const base: SessionPrs = {
    status: "ok",
    reason: null,
    opened: [],
    landed: [],
    unknown: [],
    openCount: 0,
    landedCount: 0,
    unknownCount: 0,
  };
  const merged = { ...base, ...overrides };
  return {
    ...merged,
    openCount: overrides.openCount ?? merged.opened.length,
    landedCount: overrides.landedCount ?? merged.landed.length,
    unknownCount: overrides.unknownCount ?? merged.unknown.length,
  };
}

function body(overrides: Partial<SessionInfoBody> = {}): SessionInfoBody {
  return {
    identity: {
      claudeSessionId: "8b45800b-4b65-4c15-afd4-d0feaa0cb0d4",
      terminalId: "term-1",
      fleetSessionHandle: "fsh_abc",
      tenantId: null,
      taskRunId: null,
    },
    name: { value: "runner dropdown", source: "operator" },
    account: { label: "paktis", wrapper: "clp", configDir: "C:/Users/x/.claude-paktis" },
    placement: { pageId: "default", zoneIndex: 3, workingDir: "C:/qontinui-root/qontinui-runner" },
    lifecycle: {
      state: "open",
      provider: "claude",
      origin: "authoritative",
      openedAt: 1_700_000_000_000,
      lastSeenAt: 1_700_000_100_000,
      closedAt: null,
      closeReason: null,
      confirmed: true,
      transcriptExists: true,
      restorable: true,
      restoredFromBootAt: null,
      restoreTier: null,
      bypassPermissions: false,
    },
    prs: prs(),
    ...overrides,
  };
}

const okState = (b: SessionInfoBody): SessionInfoState => ({
  status: "ok",
  reason: null,
  body: b,
});

// ---------------------------------------------------------------------------
// Migrated: sortPrsUnmergedFirst → sortPrsUnlandedFirst
// ---------------------------------------------------------------------------

describe("sortPrsUnlandedFirst", () => {
  it("puts unlanded PRs above landed ones, newest first within each group", () => {
    const landed = new Set(["qontinui/qontinui-runner#660", "qontinui/qontinui-runner#665"]);
    const sorted = sortPrsUnlandedFirst(
      [
        opened({ prNumber: 660 }),
        opened({ prNumber: 670 }),
        opened({ prNumber: 665 }),
        opened({ prNumber: 650 }),
      ],
      landed,
    );
    expect(sorted.map((p) => p.prNumber)).toEqual([670, 650, 665, 660]);
  });

  it("orders by repo name when the PR numbers collide across repos", () => {
    const sorted = sortPrsUnlandedFirst(
      [
        opened({ prNumber: 7, repo: "qontinui/zeta" }),
        opened({ prNumber: 7, repo: "qontinui/alpha" }),
      ],
      new Set(),
    );
    expect(sorted.map((p) => p.repo)).toEqual(["qontinui/alpha", "qontinui/zeta"]);
  });

  it("does not mutate its input", () => {
    const input = [opened({ prNumber: 1 }), opened({ prNumber: 2 })];
    sortPrsUnlandedFirst(input, new Set(["qontinui/qontinui-runner#1"]));
    expect(input.map((p) => p.prNumber)).toEqual([1, 2]);
  });
});

// ---------------------------------------------------------------------------
// Migrated + extended: summarizePrs (three buckets, never a partition)
// ---------------------------------------------------------------------------

describe("summarizePrs", () => {
  it("labels opened and landed as separate counts, not a split of one total", () => {
    // A PR opened AND landed in the same session appears in BOTH — 3 opened
    // with 2 of them landed must read `3 opened · 2 landed`, never `1 · 2`.
    const summary = summarizePrs(
      prs({
        opened: [opened({ prNumber: 1 }), opened({ prNumber: 2 }), opened({ prNumber: 3 })],
        landed: [
          { repo: "qontinui/qontinui-runner", prNumber: 1, landedAt: null, landSignal: "ff-land" },
          {
            repo: "qontinui/qontinui-runner",
            prNumber: 2,
            landedAt: null,
            landSignal: "github-merge",
          },
        ],
      }),
    );
    expect(summary.text).toBe("3 opened · 2 landed");
    expect(summary).toMatchObject({
      openCount: 3,
      landedCount: 2,
      unknownCount: 0,
      allLanded: false,
    });
  });

  it("appends the unknown bucket only when it is non-empty", () => {
    expect(
      summarizePrs(
        prs({
          opened: [opened({ prNumber: 1 }), opened({ prNumber: 2 })],
          landed: [
            {
              repo: "qontinui/qontinui-runner",
              prNumber: 1,
              landedAt: null,
              landSignal: "ff-land",
            },
          ],
          unknown: [{ repo: "qontinui/qontinui-runner", prNumber: 2, reason: "ref_stale" }],
        }),
      ).text,
    ).toBe("2 opened · 1 landed · 1 unknown");
  });

  it("is allLanded only when every opened PR landed and nothing is unevaluable", () => {
    const landedRow = (prNumber: number) => ({
      repo: "qontinui/qontinui-runner",
      prNumber,
      landedAt: null,
      landSignal: "ff-land",
    });
    expect(
      summarizePrs(prs({ opened: [opened({ prNumber: 1 })], landed: [landedRow(1)] })).allLanded,
    ).toBe(true);
    // One unevaluable row is enough to withhold the all-clear: the runner
    // never established that PR landed OR did not land (R1).
    expect(
      summarizePrs(
        prs({
          opened: [opened({ prNumber: 1 }), opened({ prNumber: 2 })],
          landed: [landedRow(1)],
          unknown: [
            { repo: "qontinui/qontinui-runner", prNumber: 2, reason: "head_object_missing" },
          ],
        }),
      ).allLanded,
    ).toBe(false);
  });

  it("is NOT vacuously allLanded on an empty ledger — no PRs is not all landed", () => {
    expect(summarizePrs(prs())).toMatchObject({
      openCount: 0,
      landedCount: 0,
      unknownCount: 0,
      allLanded: false,
      text: "0 opened · 0 landed",
    });
  });

  it("states the reason instead of a count when the ledger is unavailable", () => {
    expect(summarizePrs(prs({ status: "unavailable", reason: "pg_unavailable" })).text).toBe(
      "PRs unavailable — pg_unavailable",
    );
    // Missing reason is still spelled, never rendered as a bare empty.
    expect(summarizePrs(prs({ status: "unavailable", reason: null })).text).toBe(
      `PRs unavailable — ${UNKNOWN_TEXT}`,
    );
  });
});

// ---------------------------------------------------------------------------
// Migrated verbatim: prGithubUrl / prLabel
// ---------------------------------------------------------------------------

describe("prGithubUrl / prLabel", () => {
  it("links owner/name repos and shows the bare-name label", () => {
    const p = opened({ prNumber: 663 });
    expect(prGithubUrl(p)).toBe("https://github.com/qontinui/qontinui-runner/pull/663");
    expect(prLabel(p)).toBe("qontinui-runner#663");
  });

  it("refuses a link for the bare checkout-name repo skew", () => {
    const p = opened({ prNumber: 12, repo: "qontinui-runner" });
    expect(prGithubUrl(p)).toBeNull();
    expect(prLabel(p)).toBe("qontinui-runner#12");
  });

  it("keys a PR by owner/name#number", () => {
    expect(prKey(opened({ prNumber: 5 }))).toBe("qontinui/qontinui-runner#5");
  });
});

// ---------------------------------------------------------------------------
// New: the three-bucket land-signal chips
// ---------------------------------------------------------------------------

describe("prRowChip", () => {
  it("renders an ff-land as landed — a fast-forward land is NOT unmerged (G3)", () => {
    const chip = prRowChip(
      opened({ prNumber: 1, prState: "closed" }),
      { repo: "qontinui/qontinui-runner", prNumber: 1, landedAt: null, landSignal: "ff-land" },
      undefined,
    );
    expect(chip).toEqual({ text: "ff-land", tone: "landed" });
  });

  it("renders a GitHub merge as `merged`", () => {
    expect(
      prRowChip(
        opened({ prNumber: 1 }),
        {
          repo: "qontinui/qontinui-runner",
          prNumber: 1,
          landedAt: null,
          landSignal: "github-merge",
        },
        undefined,
      ),
    ).toEqual({ text: "merged", tone: "landed" });
  });

  it("carries the reason on an unevaluable verdict, and never calls it not-landed", () => {
    const chip = prRowChip(opened({ prNumber: 1, prState: "closed" }), undefined, {
      repo: "qontinui/qontinui-runner",
      prNumber: 1,
      reason: "ref_stale",
    });
    expect(chip).toEqual({ text: "unknown — ref_stale", tone: "unknown" });
  });

  it("distinguishes a closed-not-landed PR from an open one", () => {
    expect(prRowChip(opened({ prNumber: 1, prState: "closed" }), undefined, undefined)).toEqual({
      text: "closed, not landed",
      tone: "not-landed",
    });
    expect(prRowChip(opened({ prNumber: 1, prState: null }), undefined, undefined)).toEqual({
      text: "open",
      tone: "open",
    });
  });
});

describe("prPanelRows", () => {
  it("lists every bucket, unlanded first, with the right chip per row", () => {
    const rows = prPanelRows(
      prs({
        opened: [
          opened({ prNumber: 10 }),
          opened({ prNumber: 11, prState: "closed" }),
          opened({ prNumber: 12, prState: "closed" }),
        ],
        landed: [
          { repo: "qontinui/qontinui-runner", prNumber: 12, landedAt: null, landSignal: "ff-land" },
        ],
        unknown: [{ repo: "qontinui/qontinui-runner", prNumber: 11, reason: "no_base_ref" }],
      }),
    );
    expect(rows.map((r) => r.prNumber)).toEqual([11, 10, 12]);
    expect(rows.map((r) => r.chip.tone)).toEqual(["unknown", "open", "landed"]);
  });

  it("surfaces a landed row that has no matching opened row rather than dropping it", () => {
    const rows = prPanelRows(
      prs({
        opened: [],
        landed: [
          {
            repo: "qontinui/qontinui-web",
            prNumber: 99,
            landedAt: null,
            landSignal: "github-merge",
          },
        ],
        openCount: 0,
      }),
    );
    expect(rows.map((r) => r.key)).toEqual(["qontinui/qontinui-web#99"]);
    expect(rows[0].chip).toEqual({ text: "merged", tone: "landed" });
  });

  it("is empty when the ledger could not answer (the reason renders elsewhere)", () => {
    expect(prPanelRows(prs({ status: "unavailable", reason: "pg_unavailable" }))).toEqual([]);
  });
});

// ---------------------------------------------------------------------------
// New: the trigger never self-hides (G5)
// ---------------------------------------------------------------------------

describe("deriveTrigger", () => {
  it("renders a muted unknown WITH the reason instead of disappearing", () => {
    const trigger = deriveTrigger({
      status: "unavailable",
      reason: "session_not_found",
      body: null,
    });
    expect(trigger).toMatchObject({
      tone: "unknown",
      color: TONE_COLORS.unknown,
      countText: "?",
      summary: "Session info unavailable — session_not_found",
      dataSummary: "unavailable:session_not_found",
    });
  });

  it("spells a reason even when the envelope omitted one", () => {
    expect(deriveTrigger({ status: "unavailable", reason: null, body: null })).toMatchObject({
      countText: "?",
      dataSummary: `unavailable:${UNKNOWN_TEXT}`,
    });
  });

  it("distinguishes an UNREADABLE PR ledger from a session with zero PRs", () => {
    const unreadable = deriveTrigger(
      okState(body({ prs: prs({ status: "unavailable", reason: "pg_unavailable" }) })),
    );
    const zeroPrs = deriveTrigger(okState(body()));

    // The whole point of G5: these two must not render the same.
    expect(unreadable.countText).toBe("?");
    expect(zeroPrs.countText).toBe("0");
    expect(unreadable.tone).toBe("unknown");
    expect(zeroPrs.tone).toBe("empty");
    expect(unreadable.color).not.toBe(zeroPrs.color);
    expect(unreadable.dataSummary).toBe("prs-unavailable:pg_unavailable");
    expect(zeroPrs.dataSummary).toBe("0 opened · 0 landed");
  });

  it("is green only when every opened PR landed", () => {
    const allLanded = deriveTrigger(
      okState(
        body({
          prs: prs({
            opened: [opened({ prNumber: 1 })],
            landed: [
              {
                repo: "qontinui/qontinui-runner",
                prNumber: 1,
                landedAt: null,
                landSignal: "ff-land",
              },
            ],
          }),
        }),
      ),
    );
    expect(allLanded).toMatchObject({ tone: "landed", color: TONE_COLORS.landed, countText: "1" });

    const pending = deriveTrigger(
      okState(body({ prs: prs({ opened: [opened({ prNumber: 1 })] }) })),
    );
    expect(pending).toMatchObject({ tone: "pending", color: TONE_COLORS.pending });
  });

  it("goes amber, not green, while any land verdict is unevaluable", () => {
    const trigger = deriveTrigger(
      okState(
        body({
          prs: prs({
            opened: [opened({ prNumber: 1 })],
            landed: [
              {
                repo: "qontinui/qontinui-runner",
                prNumber: 1,
                landedAt: null,
                landSignal: "ff-land",
              },
            ],
            unknown: [{ repo: "qontinui/qontinui-runner", prNumber: 2, reason: "ref_stale" }],
          }),
        }),
      ),
    );
    expect(trigger.tone).toBe("partial");
    expect(trigger.dataSummary).toBe("1 opened · 1 landed · 1 unknown");
  });

  it("shows a loading state that is neither a value nor a denial", () => {
    expect(deriveTrigger({ status: "loading", reason: null, body: null })).toMatchObject({
      tone: "loading",
      countText: "…",
      dataSummary: "loading",
    });
  });
});

// ---------------------------------------------------------------------------
// New: the addressable row contract (D2 / D5) and R2's unknown rendering
// ---------------------------------------------------------------------------

describe("sessionInfoElementId", () => {
  it("is zone-indexed, matching the terminal-zone-header-<n> convention", () => {
    expect(sessionInfoElementId("trigger", 3)).toBe("terminal-session-info-trigger-3");
    expect(sessionInfoElementId("panel", 0)).toBe("terminal-session-info-panel-0");
    expect(sessionInfoElementId("claude-session-id", 2)).toBe(
      "terminal-session-info-claude-session-id-2",
    );
  });
});

describe("sessionInfoRows", () => {
  it("emits all fourteen fields, in the D5 order, all four ids among them (D2)", () => {
    expect(sessionInfoRows(body()).map((r) => r.field)).toEqual([
      "account",
      "name",
      "terminal-id",
      "claude-session-id",
      "fleet-handle",
      "tenant",
      "task-run",
      "working-dir",
      "provider",
      "origin",
      "opened-at",
      "restore-tier",
      "prs-opened",
      "prs-landed",
    ]);
  });

  it("carries the RAW value for a UI Bridge client, not the rendered text", () => {
    const rows = sessionInfoRows(body());
    const account = rows.find((r) => r.field === "account");
    expect(account?.value).toBe("paktis");
    expect(account?.display).toBe("paktis (clp)");

    const openedAt = rows.find((r) => r.field === "opened-at");
    expect(openedAt?.value).toBe("1700000000000");
    expect(openedAt?.display).toBe("2023-11-14 22:13:20Z");
  });

  it("renders `unknown` — never blank, never a guess — for a pre-P1 session (R2)", () => {
    const rows = sessionInfoRows(
      body({
        name: { value: null, source: "unknown" },
        account: { label: null, wrapper: null, configDir: null },
        identity: {
          claudeSessionId: "sid",
          terminalId: "tid",
          fleetSessionHandle: null,
          tenantId: null,
          taskRunId: null,
        },
      }),
    );
    for (const field of ["account", "name", "fleet-handle", "tenant", "task-run"] as const) {
      const row = rows.find((r) => r.field === field);
      expect(row?.value, field).toBeNull();
      expect(row?.display, field).toBe(UNKNOWN_TEXT);
    }
    // Present values are untouched by the unknown rendering.
    expect(rows.find((r) => r.field === "claude-session-id")?.value).toBe("sid");
  });

  it("states the PR reason on the count rows instead of showing a false zero", () => {
    const rows = sessionInfoRows(
      body({ prs: prs({ status: "unavailable", reason: "pg_unavailable" }) }),
    );
    for (const field of ["prs-opened", "prs-landed"] as const) {
      const row = rows.find((r) => r.field === field);
      expect(row?.value, field).toBeNull();
      expect(row?.display, field).toBe("unavailable — pg_unavailable");
    }
  });

  it("marks exactly the id-ish rows copyable", () => {
    expect(
      sessionInfoRows(body())
        .filter((r) => r.copyable)
        .map((r) => r.field),
    ).toEqual([
      "terminal-id",
      "claude-session-id",
      "fleet-handle",
      "tenant",
      "task-run",
      "working-dir",
    ]);
  });
});

describe("formatEpochMs", () => {
  it("formats a millisecond stamp and refuses to invent 1970 for an unset one", () => {
    expect(formatEpochMs(1_700_000_000_000)).toBe("2023-11-14 22:13:20Z");
    expect(formatEpochMs(0)).toBeNull();
    expect(formatEpochMs(null)).toBeNull();
    expect(formatEpochMs(Number.NaN)).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// New: normalization never produces a bare empty
// ---------------------------------------------------------------------------

describe("normalizeSessionInfo", () => {
  it("turns a degraded envelope into an unavailable state carrying its reason", () => {
    expect(normalizeSessionInfo({ available: false, reason: "session_not_found" })).toEqual({
      status: "unavailable",
      reason: "session_not_found",
      body: null,
    });
  });

  it("never returns a reasonless unavailable, whatever arrives on the wire", () => {
    for (const raw of [null, undefined, 42, "nope", {}, { available: false }]) {
      const state = normalizeSessionInfo(raw);
      expect(state.status).toBe("unavailable");
      expect(state.reason, JSON.stringify(raw ?? null)).toBeTruthy();
      expect(state.body).toBeNull();
    }
  });

  it("degrades the PR ledger alone — identity stays answerable without PG", () => {
    const b = body();
    const state = normalizeSessionInfo({
      available: true,
      identity: b.identity,
      name: b.name,
      account: b.account,
      placement: b.placement,
      lifecycle: b.lifecycle,
      prs: { status: "unavailable", reason: "pg_unavailable" },
    });
    expect(state.status).toBe("ok");
    expect(state.body?.identity.claudeSessionId).toBe(b.identity.claudeSessionId);
    expect(state.body?.prs).toMatchObject({ status: "unavailable", reason: "pg_unavailable" });
  });

  it("treats a truncated available envelope as malformed rather than half-rendering it", () => {
    expect(normalizeSessionInfo({ available: true, identity: body().identity })).toEqual({
      status: "unavailable",
      reason: "malformed_envelope",
      body: null,
    });
  });

  it("falls back to `unknown` for a name source the backend did not state", () => {
    const b = body();
    const state = normalizeSessionInfo({
      available: true,
      identity: b.identity,
      name: { value: null, source: null },
      account: b.account,
      placement: b.placement,
      lifecycle: b.lifecycle,
      prs: b.prs,
    });
    expect(state.body?.name).toEqual({ value: null, source: UNKNOWN_TEXT });
  });
});
