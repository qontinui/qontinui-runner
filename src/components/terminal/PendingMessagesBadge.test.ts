import { describe, expect, it } from "vitest";

import { paneBadge, paneBadgeText, parsePendingView } from "./PendingMessagesBadge";

const NOW = Date.parse("2026-11-27T08:00:00Z");

function view(overrides: Record<string, unknown> = {}) {
  return parsePendingView({
    state: "fresh",
    unknownReason: null,
    polledAt: "2026-11-27T07:59:55Z",
    pollAgeSecs: 5,
    staleAfterSecs: 30,
    byTerminal: {
      "term-1": [
        {
          messageId: "d5efb40e-b5f2-492c-819d-f0ceb29ce63a",
          priority: "normal",
          reason: "session_id_unbound",
          blockedSince: "2026-11-27T07:50:00Z",
        },
      ],
    },
    ...overrides,
  });
}

describe("paneBadge", () => {
  it("lists a fresh view's messages for the pane with their reason", () => {
    const badge = paneBadge(view(), "term-1", NOW);
    expect(badge.kind).toBe("pending");
    expect(paneBadgeText(badge)).toContain("d5efb40e-b5f2-492c-819d-f0ceb29ce63a");
    expect(paneBadgeText(badge)).toContain("session_id_unbound");
  });

  it("shows nothing for a pane a fresh view has no messages for", () => {
    expect(paneBadge(view(), "term-2", NOW).kind).toBe("none");
  });

  it("is unknown, never none, when the view is unknown, stale or absent", () => {
    expect(paneBadge(null, "term-2", NOW).kind).toBe("unknown");
    expect(
      paneBadge(
        view({ state: "unknown", unknownReason: "this runner is not paired with coord", byTerminal: {} }),
        "term-2",
        NOW,
      ).kind,
    ).toBe("unknown");
    expect(paneBadge(view({ polledAt: "2026-11-27T07:58:00Z" }), "term-2", NOW).kind).toBe(
      "unknown",
    );
  });

  it("ignores a malformed payload", () => {
    expect(parsePendingView({ state: "maybe" })).toBeNull();
    expect(parsePendingView(null)).toBeNull();
  });
});
