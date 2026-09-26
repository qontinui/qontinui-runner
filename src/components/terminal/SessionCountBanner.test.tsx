/**
 * Pure-helper tests for the soft max-sessions advisory (many-sessions plan
 * Phase 8).
 *
 * `environment: "node"` vitest, so this pins the exported predicates rather
 * than rendering (same precedent as `HoldingLockBanner.test.tsx`). The JSX
 * shell carries `data-ui-bridge-id` attributes for the UI Bridge smoke.
 *
 * The load-bearing property here is a NEGATIVE one: the plan's §5 rejects a
 * hard session cap, so these tests assert that the module exposes nothing
 * that could refuse a spawn — only a display predicate.
 */

import { describe, it, expect, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

vi.mock("@/lib/perfCaps", () => ({
  usePerfCaps: () => ({ max_sessions_warn: 30 }),
}));

import * as bannerModule from "./SessionCountBanner";
import { shouldShowSessionCountBanner, shouldResetDismissal } from "./SessionCountBanner";

describe("shouldShowSessionCountBanner", () => {
  it("stays quiet below the threshold", () => {
    expect(shouldShowSessionCountBanner(29, 30, false)).toBe(false);
    expect(shouldShowSessionCountBanner(0, 30, false)).toBe(false);
  });

  it("warns at and past the threshold", () => {
    expect(shouldShowSessionCountBanner(30, 30, false)).toBe(true);
    expect(shouldShowSessionCountBanner(31, 30, false)).toBe(true);
    expect(shouldShowSessionCountBanner(500, 30, false)).toBe(true);
  });

  it("stays dismissed once dismissed, however far past the threshold", () => {
    expect(shouldShowSessionCountBanner(30, 30, true)).toBe(false);
    expect(shouldShowSessionCountBanner(120, 30, true)).toBe(false);
  });

  it("treats a zero threshold as 'warn from the first session', not as off", () => {
    // There is deliberately no disable switch — the knob is a display
    // threshold, and disabling it would be the first step toward a cap.
    expect(shouldShowSessionCountBanner(1, 0, false)).toBe(true);
  });

  it("never warns with nothing open, however low the threshold", () => {
    // `0 >= 0` is true, but "0 panes open" on an empty terminal page is
    // noise, not advice.
    expect(shouldShowSessionCountBanner(0, 0, false)).toBe(false);
    expect(shouldShowSessionCountBanner(0, 30, false)).toBe(false);
    expect(shouldShowSessionCountBanner(-1, 0, false)).toBe(false);
  });
});

describe("shouldResetDismissal", () => {
  it("re-arms only after dropping back under the threshold", () => {
    expect(shouldResetDismissal(29, 30)).toBe(true);
    expect(shouldResetDismissal(30, 30)).toBe(false);
    expect(shouldResetDismissal(45, 30)).toBe(false);
  });
});

describe("the rail warns and never refuses", () => {
  /**
   * The whole module's surface is: a component, a display predicate, and a
   * re-arm predicate. If a gate/allow/refuse helper ever appears here, this
   * fails — which is the point. §5 of the plan calls a hard session cap a
   * capability regression.
   */
  it("exports no gating helper", () => {
    expect(Object.keys(bannerModule).sort()).toEqual([
      "SessionCountBanner",
      "shouldResetDismissal",
      "shouldShowSessionCountBanner",
    ]);
  });

  /**
   * Both predicates are total over the count domain and never signal
   * "refuse": the only outputs are booleans about VISIBILITY, and a caller
   * cannot derive a spawn veto from them because showing the banner is
   * independent of whether another session may be opened.
   */
  it("answers a visibility question for every count, including absurd ones", () => {
    for (const count of [0, 1, 29, 30, 31, 1000, Number.MAX_SAFE_INTEGER]) {
      expect(typeof shouldShowSessionCountBanner(count, 30, false)).toBe("boolean");
      expect(typeof shouldResetDismissal(count, 30)).toBe("boolean");
    }
  });
});

describe("the banner labels its population", () => {
  /**
   * The banner is fed `tabs.length` — open PTY panes — while the status strip
   * on the same page prints "N sessions" from the Claude-session model. Two
   * numbers both labelled "sessions" stacked on one page was the defect; the
   * banner's copy must name panes.
   */
  it('reads "N panes open", never "sessions open"', () => {
    const html = renderToStaticMarkup(
      <bannerModule.SessionCountBanner openPaneCount={4} threshold={4} />,
    );
    expect(html).toContain("4 panes open");
    expect(html).not.toContain("sessions open");
    expect(html).toContain('data-open-pane-count="4"');
  });

  it("renders nothing below the threshold (negative control)", () => {
    expect(
      renderToStaticMarkup(<bannerModule.SessionCountBanner openPaneCount={3} threshold={4} />),
    ).toBe("");
  });
});
