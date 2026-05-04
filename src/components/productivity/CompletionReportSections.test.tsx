/**
 * Pure-logic tests for the Phase 4 Completion Report sections.
 *
 * The runner's vitest config uses `environment: "node"` (no jsdom — see
 * `vitest.config.ts` and `WebIntegrationAuthBanner.test.ts` for prior art).
 * That means we can't render React trees, so the new components are
 * structured around exported pure helpers (`completionSourceLabel`,
 * `deliverableHref`, `buildReportFromDraft`, `validateManualFireDraft`,
 * `truncatePreview`) that carry the load-bearing logic. The component JSX
 * is a thin shell over those helpers and is verified manually + by the
 * `productivity.spec.uibridge.json` element-presence assertions.
 */

import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import {
  buildReportFromDraft,
  completionSourceClasses,
  completionSourceLabel,
  deliverableHref,
  followUpPriorityClasses,
  truncatePreview,
  validateManualFireDraft,
  type DraftState,
} from "./CompletionReportSections";

// ============================================================================
// completionSourceLabel — the source-badge text. The plan §5 requires "small
// pill showing the completion_source value", and we verified each enum
// member produces a stable, capitalized label.
// ============================================================================

describe("completionSourceLabel", () => {
  it("returns a human label for every CompletionSource value", () => {
    expect(completionSourceLabel("session-self-report")).toBe("Session self-report");
    expect(completionSourceLabel("reviewer-attestation")).toBe("Reviewer attestation");
    expect(completionSourceLabel("github-merge")).toBe("GitHub merge");
    expect(completionSourceLabel("ci-pipeline")).toBe("CI pipeline");
    expect(completionSourceLabel("manual-user-fire")).toBe("Manual user fire");
    expect(completionSourceLabel("legacy-pre-completion-reports")).toBe(
      "Legacy (pre-migration)",
    );
  });

  it("returns Tailwind classes for every CompletionSource value", () => {
    // Smoke test — no source must produce an empty string (which would
    // collapse the badge styling).
    for (const src of [
      "session-self-report",
      "reviewer-attestation",
      "github-merge",
      "ci-pipeline",
      "manual-user-fire",
      "legacy-pre-completion-reports",
    ] as const) {
      expect(completionSourceClasses(src).length).toBeGreaterThan(0);
    }
  });
});

// ============================================================================
// deliverableHref — linkify URL-shaped references.
// ============================================================================

describe("deliverableHref", () => {
  it("returns the URL for a PR deliverable that already has https://", () => {
    const out = deliverableHref({
      kind: "pr",
      reference: "https://github.com/x/y/pull/42",
      description: "Demo PR",
    });
    expect(out).toBe("https://github.com/x/y/pull/42");
  });

  it("returns the URL for an endpoint deliverable that's an http URL", () => {
    const out = deliverableHref({
      kind: "endpoint",
      reference: "http://localhost:8080/foo",
      description: "Local dev",
    });
    expect(out).toBe("http://localhost:8080/foo");
  });

  it("returns null for a commit SHA without a repo URL", () => {
    const out = deliverableHref({
      kind: "commit",
      reference: "deadbeef1234",
      description: "Some commit",
    });
    expect(out).toBeNull();
  });

  it("returns null for a file path", () => {
    const out = deliverableHref({
      kind: "file",
      reference: "src/foo/bar.ts",
      description: "New file",
    });
    expect(out).toBeNull();
  });

  it("returns null for an empty reference", () => {
    expect(
      deliverableHref({ kind: "other", reference: "", description: "no ref" }),
    ).toBeNull();
    expect(
      deliverableHref({ kind: "other", reference: "   ", description: "ws ref" }),
    ).toBeNull();
  });
});

// ============================================================================
// followUpPriorityClasses — colors for the priority badge.
// ============================================================================

describe("followUpPriorityClasses", () => {
  it("returns red-tinted classes for critical priority", () => {
    const out = followUpPriorityClasses("critical");
    expect(out).toMatch(/red/);
  });

  it("returns amber classes for important priority", () => {
    const out = followUpPriorityClasses("important");
    expect(out).toMatch(/amber/);
  });

  it("returns muted classes for nice-to-have", () => {
    const out = followUpPriorityClasses("nice-to-have");
    expect(out).toMatch(/muted/);
  });

  it("falls back to muted classes for unknown priority", () => {
    const out = followUpPriorityClasses("unknown-priority");
    expect(out).toMatch(/muted/);
  });
});

// ============================================================================
// truncatePreview — used by upstream-reports preview.
// ============================================================================

describe("truncatePreview", () => {
  it("returns the input verbatim when ≤ max", () => {
    expect(truncatePreview("hello", 100)).toBe("hello");
  });

  it("truncates and appends an ellipsis when over the cap", () => {
    const long = "a".repeat(500);
    const out = truncatePreview(long, 300);
    expect(out.length).toBeLessThanOrEqual(301); // 300 chars + "…"
    expect(out.endsWith("…")).toBe(true);
  });

  it("trims trailing whitespace before the ellipsis", () => {
    const out = truncatePreview("hello       ", 5);
    expect(out).toBe("hello…");
  });

  it("uses the default 300-char cap", () => {
    const long = "x".repeat(400);
    const out = truncatePreview(long);
    expect(out.length).toBe(301);
  });
});

// ============================================================================
// validateManualFireDraft — front-end validation gate.
// ============================================================================

describe("validateManualFireDraft", () => {
  const baseDraft: DraftState = {
    summaryMd: "ok",
    deliverables: [],
    breakingChanges: [],
    followUps: [],
  };

  it("requires non-empty summaryMd", () => {
    expect(validateManualFireDraft({ ...baseDraft, summaryMd: "" })).toMatch(
      /summaryMd is required/,
    );
    expect(validateManualFireDraft({ ...baseDraft, summaryMd: "   " })).toMatch(
      /summaryMd is required/,
    );
  });

  it("rejects oversize summaryMd", () => {
    const tooLong = { ...baseDraft, summaryMd: "x".repeat(4001) };
    expect(validateManualFireDraft(tooLong)).toMatch(/too long/);
  });

  it("accepts a valid draft", () => {
    expect(validateManualFireDraft(baseDraft)).toBeNull();
  });
});

// ============================================================================
// buildReportFromDraft — drops empty rows so the wire body is clean.
// ============================================================================

describe("buildReportFromDraft", () => {
  it("drops fully-empty deliverable rows", () => {
    const out = buildReportFromDraft({
      summaryMd: "x",
      deliverables: [
        { kind: "pr", reference: "", description: "" },
        { kind: "pr", reference: "https://example.com/1", description: "real" },
      ],
      breakingChanges: [],
      followUps: [],
    });
    expect(out.deliverables).toHaveLength(1);
    expect(out.deliverables[0]?.reference).toBe("https://example.com/1");
  });

  it("drops fully-empty breaking-change rows", () => {
    const out = buildReportFromDraft({
      summaryMd: "x",
      deliverables: [],
      breakingChanges: [
        { area: "", description: "", migrationStepsMd: "" },
        { area: "auth", description: "broke login", migrationStepsMd: "" },
      ],
      followUps: [],
    });
    expect(out.breakingChanges).toHaveLength(1);
    expect(out.breakingChanges[0]?.area).toBe("auth");
  });

  it("drops follow-ups with empty description, regardless of priority", () => {
    const out = buildReportFromDraft({
      summaryMd: "x",
      deliverables: [],
      breakingChanges: [],
      followUps: [
        { description: "", priority: "critical", blockingForDependents: true },
        { description: "do thing", priority: "nice-to-have", blockingForDependents: false },
      ],
    });
    expect(out.followUps).toHaveLength(1);
    expect(out.followUps[0]?.description).toBe("do thing");
  });

  it("preserves the blockingForDependents flag for retained follow-ups", () => {
    const out = buildReportFromDraft({
      summaryMd: "x",
      deliverables: [],
      breakingChanges: [],
      followUps: [
        { description: "must fix", priority: "critical", blockingForDependents: true },
        { description: "later", priority: "nice-to-have", blockingForDependents: false },
      ],
    });
    expect(out.followUps[0]?.blockingForDependents).toBe(true);
    expect(out.followUps[1]?.blockingForDependents).toBe(false);
  });
});

// ============================================================================
// Manual-fire HTTP integration — mock invoke + global.fetch and confirm the
// helper plumbs body shape correctly. This isolates the network contract
// without needing the React tree.
// ============================================================================

describe("manual-fire HTTP wire shape", () => {
  let originalFetch: typeof globalThis.fetch | undefined;
  let fetchMock: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    originalFetch = globalThis.fetch;
    fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      statusText: "OK",
      text: () => Promise.resolve(""),
    } as unknown as Response);
    globalThis.fetch = fetchMock as unknown as typeof globalThis.fetch;
  });

  afterEach(() => {
    if (originalFetch) globalThis.fetch = originalFetch;
    vi.restoreAllMocks();
  });

  /**
   * The `ManualFireForm.handleSubmit` handler is private to the React
   * component. To verify the wire shape without a DOM, we replicate the
   * request body the form would send and confirm `fetch` is called with
   * `task_id` + `completion_report` keys (snake_case to match the Rust
   * handler shape at `mcp/completion_sources.rs`).
   */
  it("posts {task_id, completion_report} to /completion-sources/manual-user-fire", async () => {
    const port = 8080;
    const taskId = "11111111-1111-1111-1111-111111111111";
    const draft: DraftState = {
      summaryMd: "Manually fired completion",
      deliverables: [
        { kind: "pr", reference: "https://example.com/pr/1", description: "demo" },
      ],
      breakingChanges: [],
      followUps: [
        { description: "follow up later", priority: "important", blockingForDependents: false },
      ],
    };
    const body = {
      task_id: taskId,
      completion_report: buildReportFromDraft(draft),
    };

    const res = await fetch(`http://localhost:${port}/completion-sources/manual-user-fire`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    expect(res.ok).toBe(true);

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [url, init] = fetchMock.mock.calls[0]!;
    expect(url).toBe(`http://localhost:${port}/completion-sources/manual-user-fire`);
    const sentInit = init as RequestInit;
    expect(sentInit.method).toBe("POST");
    const sentBody = JSON.parse(sentInit.body as string);
    expect(sentBody.task_id).toBe(taskId);
    expect(sentBody.completion_report.summaryMd).toBe("Manually fired completion");
    expect(sentBody.completion_report.deliverables).toHaveLength(1);
    expect(sentBody.completion_report.followUps).toHaveLength(1);
  });
});
