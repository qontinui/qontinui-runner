/**
 * The standing skip note beside the two bulk-restore buttons (the per-page
 * bar and the global strip), rendered to static markup under
 * `environment: "node"` (same precedent as `RestartPrepPanel.test.tsx`).
 *
 * Pinned (E2E re-run): a bulk button with nothing restorable is disabled and
 * used to say nothing at all; the note says what it skips whether or not the
 * button is enabled, and it is an element with a `data-ui-bridge-id`
 * (`role="status"`), so the UI Bridge snapshot sees it.
 */
import { describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import type { RestorePlaceholder } from "./restorePlaceholders";

vi.mock("@qontinui/ui-bridge", () => ({ useUIComponent: () => ({}) }));

let rows: RestorePlaceholder[] = [];
vi.mock("./useRestoreQueue", async (importOriginal) => {
  const real = await importOriginal<typeof import("./useRestoreQueue")>();
  return {
    ...real,
    useRestoreCohort: () => ({
      prepareId: "prep-1",
      unread: false,
      holderSweepPending: false,
      startedAt: null,
      rows,
      statusLoaded: true,
    }),
    useBulkRestore: () => () => undefined,
  };
});
vi.mock("./useRestorePlaceholderBinding", () => ({ useRestoreCohortRefresh: () => undefined }));
vi.mock("@/hooks/useCoordDrainState", () => ({ useCoordDrainState: () => null }));
vi.mock("./RunnerDrainBanner", () => ({ drainBannerModel: () => null }));
vi.mock("./useTerminalPages", () => ({ loadKnownPageIds: () => [] }));

import { RestorePageBar } from "./RestorePageBar";
import { RestoreCohortStrip } from "./RestoreCohortStrip";
import { bulkNoteText, previewBulk } from "./useRestoreQueue";

const row = (sid: string, o: Partial<RestorePlaceholder> = {}): RestorePlaceholder => ({
  claudeSessionId: sid,
  prepareId: "prep-1",
  pageId: "p1",
  zoneIndex: 0,
  name: sid,
  accountLabel: "ben",
  configDir: "/cfg",
  workingDir: "/w",
  planSlug: null,
  effortLevel: null,
  effortSource: "unknown",
  held: false,
  finished: false,
  stateAtPrepare: "idle",
  limitResetAt: null,
  blocker: null,
  ...o,
});

const heldAndLimited = () => [
  row("h", { held: true }),
  row("l", { stateAtPrepare: "limit_hit", limitResetAt: Date.now() + 3_600_000 }),
];

describe("the bulk skip note", () => {
  it("says nothing restores, and why, when every row is skipped", () => {
    const r = previewBulk(heldAndLimited());
    expect(r).toEqual({
      enqueued: 0,
      skippedHeld: 1,
      skippedFinished: 0,
      skippedBlocked: 0,
      skippedLimitHit: 1,
    });
    expect(bulkNoteText(r)).toBe(
      "Nothing to restore — 1 held, 1 usage-limited (restore those by hand)",
    );
  });

  it("names what a click would skip when some rows restore, and nothing when none skip", () => {
    expect(bulkNoteText(previewBulk([row("a"), row("f", { finished: true })]))).toBe(
      "Skips 1 finished (restore those by hand)",
    );
    expect(bulkNoteText(previewBulk([row("a"), row("b")]))).toBeNull();
  });

  it("is a queryable element beside the DISABLED per-page button", () => {
    rows = heldAndLimited();
    const html = renderToStaticMarkup(<RestorePageBar pageId="p1" notify={() => undefined} />);
    expect(html).toMatch(/data-ui-bridge-id="terminal\.restore-page-p1"[^>]*disabled/);
    expect(html).toContain('data-ui-bridge-id="terminal.restore-page-note-p1"');
    expect(html).toContain('role="status"');
    expect(html).toContain("Nothing to restore — 1 held, 1 usage-limited");
  });

  it("is a queryable element on the global strip too", () => {
    rows = heldAndLimited();
    const html = renderToStaticMarkup(<RestoreCohortStrip notify={() => undefined} />);
    expect(html).toContain('data-ui-bridge-id="terminal.restore-cohort-note"');
    expect(html).toContain("Nothing to restore — 1 held, 1 usage-limited");
  });

  it("renders no note when nothing is skipped", () => {
    rows = [row("a")];
    const html = renderToStaticMarkup(<RestorePageBar pageId="p1" notify={() => undefined} />);
    expect(html).not.toContain("restore-page-note");
  });
});
