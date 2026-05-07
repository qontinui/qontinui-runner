/**
 * Change Tracking Handler Tests (Runner — snake_case commands)
 *
 * Tests the extracted command dispatch logic that maps snake_case actions
 * to ChangeTracker method calls.
 */

import { describe, it, expect, beforeEach, vi } from "vitest";
import {
  handleChangeTrackingCommand,
  type ChangeTrackerLike,
  type ChangeTrackingDeps,
} from "./changeTrackingHandler";

// ============================================================================
// Helpers
// ============================================================================

function createMockTracker(overrides?: Partial<ChangeTrackerLike>): ChangeTrackerLike {
  return {
    saveBookmark: vi.fn().mockReturnValue({ name: "test", snapshot: {} }),
    getBookmark: vi.fn().mockReturnValue({ name: "test", snapshot: {}, timestamp: 123 }),
    deleteBookmark: vi.fn().mockReturnValue(true),
    listBookmarks: vi.fn().mockReturnValue(["a", "b"]),
    diffFromBookmark: vi
      .fn()
      .mockReturnValue({ changes: { appeared: [], disappeared: [], modified: [] } }),
    executeWithDiff: vi.fn().mockResolvedValue({ actionResult: {}, diff: null }),
    waitForChange: vi.fn().mockResolvedValue(null),
    categorizeLastDiff: vi.fn().mockReturnValue({ category: "no-op", confidence: 1, diff: null }),
    scopedDiffFromBookmark: vi.fn().mockReturnValue(null),
    summarizeDiff: vi.fn().mockReturnValue("2 elements changed"),
    enableBuffer: vi.fn(),
    disableBuffer: vi.fn(),
    drainBuffer: vi.fn().mockReturnValue({ changes: [], count: 0 }),
    getBufferSize: vi.fn().mockReturnValue(0),
    isBufferEnabled: vi.fn().mockReturnValue(false),
    ...overrides,
  };
}

function createMockDeps(overrides?: Partial<ChangeTrackingDeps>): ChangeTrackingDeps {
  return {
    createSnapshot: vi.fn().mockReturnValue({
      elements: [
        {
          id: "btn-1",
          type: "button",
          label: "Save",
          actions: ["click"],
          state: { visible: true },
        },
      ],
    }),
    createSnapshotManager: vi.fn().mockReturnValue({
      createSnapshot: vi.fn().mockReturnValue({ snapshotId: "snap-1" }),
    }),
    analyzeStructuredChanges: vi.fn().mockReturnValue({
      hasStructuredData: false,
      tableChanges: [],
      listChanges: [],
    }),
    ...overrides,
  };
}

// ============================================================================
// Tests
// ============================================================================

describe("handleChangeTrackingCommand (runner/snake_case)", () => {
  let ct: ChangeTrackerLike;
  let deps: ChangeTrackingDeps;

  beforeEach(() => {
    ct = createMockTracker();
    deps = createMockDeps();
  });

  // =========================================================================
  // Bookmark CRUD
  // =========================================================================

  describe("bookmark operations", () => {
    it("save_bookmark delegates with name", async () => {
      await handleChangeTrackingCommand(ct, "save_bookmark", { name: "snap1" }, deps);
      expect(ct.saveBookmark).toHaveBeenCalledWith("snap1");
    });

    it("get_bookmark returns bookmark when found", async () => {
      const bookmark = { name: "snap1", snapshot: { id: "s1" }, timestamp: 1 };
      (ct.getBookmark as ReturnType<typeof vi.fn>).mockReturnValue(bookmark);
      const result = await handleChangeTrackingCommand(ct, "get_bookmark", { name: "snap1" }, deps);
      expect(result).toBe(bookmark);
    });

    it("get_bookmark throws when not found", async () => {
      (ct.getBookmark as ReturnType<typeof vi.fn>).mockReturnValue(null);
      await expect(
        handleChangeTrackingCommand(ct, "get_bookmark", { name: "missing" }, deps),
      ).rejects.toThrow("Bookmark 'missing' not found");
    });

    it("delete_bookmark wraps result in { deleted }", async () => {
      (ct.deleteBookmark as ReturnType<typeof vi.fn>).mockReturnValue(true);
      const result = await handleChangeTrackingCommand(
        ct,
        "delete_bookmark",
        { name: "old" },
        deps,
      );
      expect(result).toEqual({ deleted: true });
      expect(ct.deleteBookmark).toHaveBeenCalledWith("old");
    });

    it("delete_bookmark returns false when not found", async () => {
      (ct.deleteBookmark as ReturnType<typeof vi.fn>).mockReturnValue(false);
      const result = await handleChangeTrackingCommand(
        ct,
        "delete_bookmark",
        { name: "nope" },
        deps,
      );
      expect(result).toEqual({ deleted: false });
    });

    it("list_bookmarks delegates directly", async () => {
      const names = ["a", "b", "c"];
      (ct.listBookmarks as ReturnType<typeof vi.fn>).mockReturnValue(names);
      const result = await handleChangeTrackingCommand(ct, "list_bookmarks", {}, deps);
      expect(result).toBe(names);
    });
  });

  // =========================================================================
  // Diff Operations
  // =========================================================================

  describe("diff operations", () => {
    it("diff_from_bookmark delegates with name", async () => {
      const diff = { changes: { appeared: [{ id: "e1" }], disappeared: [], modified: [] } };
      (ct.diffFromBookmark as ReturnType<typeof vi.fn>).mockReturnValue(diff);
      const result = await handleChangeTrackingCommand(
        ct,
        "diff_from_bookmark",
        { name: "snap1" },
        deps,
      );
      expect(result).toBe(diff);
      expect(ct.diffFromBookmark).toHaveBeenCalledWith("snap1");
    });

    it("execute_with_diff passes entire payload", async () => {
      const payload = {
        elementId: "btn-1",
        action: "click",
        settleTimeout: 2000,
      };
      await handleChangeTrackingCommand(ct, "execute_with_diff", payload, deps);
      expect(ct.executeWithDiff).toHaveBeenCalledWith(payload);
    });

    it("wait_for_change passes predicate and options", async () => {
      const predicate = { minChanges: 1 };
      const options = { timeout: 5000 };
      await handleChangeTrackingCommand(ct, "wait_for_change", { predicate, options }, deps);
      expect(ct.waitForChange).toHaveBeenCalledWith(predicate, options);
    });

    it("wait_for_change works without options", async () => {
      const predicate = { minChanges: 1 };
      await handleChangeTrackingCommand(ct, "wait_for_change", { predicate }, deps);
      expect(ct.waitForChange).toHaveBeenCalledWith(predicate, undefined);
    });

    it("categorize_last_diff delegates directly", async () => {
      const categorized = { category: "content-update", confidence: 0.9, diff: {} };
      (ct.categorizeLastDiff as ReturnType<typeof vi.fn>).mockReturnValue(categorized);
      const result = await handleChangeTrackingCommand(ct, "categorize_last_diff", {}, deps);
      expect(result).toBe(categorized);
    });
  });

  // =========================================================================
  // Scoped Diff
  // =========================================================================

  describe("scoped_diff", () => {
    it("delegates to scopedDiffFromBookmark when fromBookmark provided", async () => {
      const scopedDiff = { changes: {} };
      (ct.scopedDiffFromBookmark as ReturnType<typeof vi.fn>).mockReturnValue(scopedDiff);
      const result = await handleChangeTrackingCommand(
        ct,
        "scoped_diff",
        { scope: ".sidebar", fromBookmark: "snap1" },
        deps,
      );
      expect(ct.scopedDiffFromBookmark).toHaveBeenCalledWith("snap1", ".sidebar");
      expect(result).toBe(scopedDiff);
    });

    it("returns null when fromBookmark is not provided", async () => {
      const result = await handleChangeTrackingCommand(
        ct,
        "scoped_diff",
        { scope: ".sidebar" },
        deps,
      );
      expect(result).toBeNull();
      expect(ct.scopedDiffFromBookmark).not.toHaveBeenCalled();
    });
  });

  // =========================================================================
  // Summarize Diff
  // =========================================================================

  describe("summarize_diff", () => {
    it("uses fromBookmark when provided", async () => {
      const diff = { changes: {} };
      (ct.diffFromBookmark as ReturnType<typeof vi.fn>).mockReturnValue(diff);
      (ct.summarizeDiff as ReturnType<typeof vi.fn>).mockReturnValue("1 button appeared");

      const result = await handleChangeTrackingCommand(
        ct,
        "summarize_diff",
        { budget: 200, fromBookmark: "snap1", includeIds: true },
        deps,
      );

      expect(ct.diffFromBookmark).toHaveBeenCalledWith("snap1");
      expect(ct.summarizeDiff).toHaveBeenCalledWith(diff, {
        budget: 200,
        includeIds: true,
        includeCategory: undefined,
      });
      expect(result).toEqual({ summary: "1 button appeared" });
    });

    it("falls back to categorizeLastDiff when no fromBookmark", async () => {
      const diff = { changes: {} };
      (ct.categorizeLastDiff as ReturnType<typeof vi.fn>).mockReturnValue({
        category: "content-update",
        diff,
      });
      (ct.summarizeDiff as ReturnType<typeof vi.fn>).mockReturnValue("content changed");

      const result = await handleChangeTrackingCommand(ct, "summarize_diff", { budget: 100 }, deps);

      expect(ct.categorizeLastDiff).toHaveBeenCalled();
      expect(ct.summarizeDiff).toHaveBeenCalledWith(diff, {
        budget: 100,
        includeIds: undefined,
        includeCategory: undefined,
      });
      expect(result).toEqual({ summary: "content changed" });
    });

    it("returns 'No changes detected' when no diff available", async () => {
      (ct.categorizeLastDiff as ReturnType<typeof vi.fn>).mockReturnValue(null);

      const result = await handleChangeTrackingCommand(ct, "summarize_diff", { budget: 100 }, deps);

      expect(result).toEqual({ summary: "No changes detected" });
      expect(ct.summarizeDiff).not.toHaveBeenCalled();
    });

    it("returns 'No changes detected' when categorizeLastDiff has null diff", async () => {
      (ct.categorizeLastDiff as ReturnType<typeof vi.fn>).mockReturnValue({
        category: "no-op",
        diff: null,
      });

      const result = await handleChangeTrackingCommand(ct, "summarize_diff", { budget: 100 }, deps);

      expect(result).toEqual({ summary: "No changes detected" });
    });
  });

  // =========================================================================
  // Structured Changes
  // =========================================================================

  describe("structured_changes", () => {
    it("analyzes from bookmark when provided", async () => {
      const bookmark = { name: "snap1", snapshot: { snapshotId: "s1" } };
      (ct.getBookmark as ReturnType<typeof vi.fn>).mockReturnValue(bookmark);
      const analysis = {
        hasStructuredData: true,
        tableChanges: [{ type: "row-added" }],
        listChanges: [],
      };
      (deps.analyzeStructuredChanges as ReturnType<typeof vi.fn>).mockReturnValue(analysis);

      const result = await handleChangeTrackingCommand(
        ct,
        "structured_changes",
        { fromBookmark: "snap1" },
        deps,
      );

      expect(ct.getBookmark).toHaveBeenCalledWith("snap1");
      expect(deps.createSnapshot).toHaveBeenCalled();
      expect(deps.createSnapshotManager).toHaveBeenCalledWith({});
      expect(deps.analyzeStructuredChanges).toHaveBeenCalledWith(
        bookmark.snapshot,
        expect.anything(),
      );
      expect(result).toBe(analysis);
    });

    it("throws when bookmark not found", async () => {
      (ct.getBookmark as ReturnType<typeof vi.fn>).mockReturnValue(null);

      await expect(
        handleChangeTrackingCommand(ct, "structured_changes", { fromBookmark: "missing" }, deps),
      ).rejects.toThrow("Bookmark 'missing' not found");
    });

    it("returns empty analysis without fromBookmark", async () => {
      const result = await handleChangeTrackingCommand(ct, "structured_changes", {}, deps);

      expect(result).toEqual({
        hasStructuredData: false,
        tableChanges: [],
        listChanges: [],
      });
      expect(ct.getBookmark).not.toHaveBeenCalled();
    });
  });

  // =========================================================================
  // Change Buffer
  // =========================================================================

  describe("change buffer operations", () => {
    it("enable_change_buffer enables and returns status", async () => {
      const result = await handleChangeTrackingCommand(ct, "enable_change_buffer", {}, deps);
      expect(ct.enableBuffer).toHaveBeenCalled();
      expect(result).toEqual({ enabled: true });
    });

    it("disable_change_buffer disables and returns status", async () => {
      const result = await handleChangeTrackingCommand(ct, "disable_change_buffer", {}, deps);
      expect(ct.disableBuffer).toHaveBeenCalled();
      expect(result).toEqual({ enabled: false });
    });

    it("drain_change_buffer delegates directly", async () => {
      const drained = { changes: [{ id: "c1" }], count: 1 };
      (ct.drainBuffer as ReturnType<typeof vi.fn>).mockReturnValue(drained);

      const result = await handleChangeTrackingCommand(ct, "drain_change_buffer", {}, deps);
      expect(result).toBe(drained);
    });

    it("get_change_buffer_size returns size and enabled status", async () => {
      (ct.getBufferSize as ReturnType<typeof vi.fn>).mockReturnValue(5);
      (ct.isBufferEnabled as ReturnType<typeof vi.fn>).mockReturnValue(true);

      const result = await handleChangeTrackingCommand(ct, "get_change_buffer_size", {}, deps);
      expect(result).toEqual({ size: 5, enabled: true });
    });

    // -----------------------------------------------------------------------
    // Phase E (plan 2026-05-07): get_changes_since + get_element_history
    // back ChangeTracker.changeBuffer via peekBuffer().
    // -----------------------------------------------------------------------

    it("get_changes_since filters by recordedAt and respects limit", async () => {
      const fixture = [
        { recordedAt: 1, sequence: 0 },
        { recordedAt: 5, sequence: 1 },
        { recordedAt: 10, sequence: 2 },
        { recordedAt: 15, sequence: 3 },
      ];
      ct.peekBuffer = vi.fn().mockReturnValue(fixture);

      const result = (await handleChangeTrackingCommand(
        ct,
        "get_changes_since",
        { params: { since: "5", limit: "10" } },
        deps,
      )) as { events: Array<{ recordedAt: number }>; count: number };

      expect(result.count).toBe(2);
      expect(result.events.map((e) => e.recordedAt)).toEqual([10, 15]);
    });

    it("get_changes_since defaults to since=0, limit=100 when params missing", async () => {
      const fixture = [
        { recordedAt: 1, sequence: 0 },
        { recordedAt: 2, sequence: 1 },
      ];
      ct.peekBuffer = vi.fn().mockReturnValue(fixture);

      const result = (await handleChangeTrackingCommand(
        ct,
        "get_changes_since",
        {},
        deps,
      )) as { events: unknown[]; count: number };

      expect(result.count).toBe(2);
    });

    it("get_changes_since returns empty when peekBuffer is unavailable", async () => {
      // Older SDK builds — peekBuffer not on the tracker. The handler
      // falls back to [] and never throws.
      delete (ct as { peekBuffer?: unknown }).peekBuffer;

      const result = (await handleChangeTrackingCommand(
        ct,
        "get_changes_since",
        { params: { since: 0, limit: 50 } },
        deps,
      )) as { events: unknown[]; count: number };

      expect(result).toEqual({ events: [], count: 0 });
    });

    it("get_element_history returns BufferedChange entries that mention the element id", async () => {
      const target = "btn-save";
      const fixture = [
        // Matches via diff.changes.modified
        {
          recordedAt: 100,
          diff: {
            changes: { modified: [{ elementId: target }], appeared: [], disappeared: [] },
          },
        },
        // Matches via diff.contentChanges.textChanges
        {
          recordedAt: 200,
          diff: {
            changes: {},
            contentChanges: {
              textChanges: [{ elementId: target }],
              metricChanges: [],
              statusChanges: [],
            },
          },
        },
        // Different element — must be excluded
        {
          recordedAt: 300,
          diff: {
            changes: { modified: [{ elementId: "other-id" }] },
          },
        },
        // Route-change — must be excluded by type discriminator
        {
          recordedAt: 400,
          type: "route-change",
          from: "/a",
          to: "/b",
        },
      ];
      ct.peekBuffer = vi.fn().mockReturnValue(fixture);

      const result = (await handleChangeTrackingCommand(
        ct,
        "get_element_history",
        { params: { id: target } },
        deps,
      )) as Array<{ recordedAt: number }>;

      expect(result).toHaveLength(2);
      expect(result.map((e) => e.recordedAt)).toEqual([100, 200]);
    });

    it("get_element_history throws when id is missing", async () => {
      ct.peekBuffer = vi.fn().mockReturnValue([]);
      await expect(
        handleChangeTrackingCommand(ct, "get_element_history", {}, deps),
      ).rejects.toThrow(/requires an 'id' parameter/);
    });
  });

  // =========================================================================
  // Unknown Commands
  // =========================================================================

  describe("unknown commands", () => {
    it("returns undefined for unknown action (runner convention)", async () => {
      const result = await handleChangeTrackingCommand(ct, "unknown_action", {}, deps);
      expect(result).toBeUndefined();
    });
  });
});
