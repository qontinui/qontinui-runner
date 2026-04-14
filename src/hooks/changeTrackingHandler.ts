/**
 * Change Tracking Command Handler
 *
 * Extracted from useUIBridgeEventHandler.ts for testability.
 * Dispatches snake_case change tracking commands to a ChangeTracker instance.
 */

// ---------------------------------------------------------------------------
// Minimal interface describing the ChangeTracker methods used by dispatch.
// ---------------------------------------------------------------------------

export interface ChangeTrackerLike {
  saveBookmark(name: string): unknown;
  getBookmark(name: string): { snapshot: unknown } | null;
  deleteBookmark(name: string): boolean;
  listBookmarks(): unknown;
  diffFromBookmark(name: string): unknown;
  executeWithDiff(request: unknown): Promise<unknown>;
  waitForChange(predicate: unknown, options?: unknown): Promise<unknown>;
  categorizeLastDiff(): { diff: unknown } | null;
  scopedDiffFromBookmark(bookmarkName: string, scope: string): unknown;
  summarizeDiff(
    diff: unknown,
    options: {
      budget: number;
      includeIds?: boolean;
      includeCategory?: boolean;
    },
  ): string;
  enableBuffer(): void;
  disableBuffer(): void;
  drainBuffer(): unknown;
  getBufferSize(): number;
  isBufferEnabled(): boolean;
  /** P1.3 — push a SPA route-change entry into the change buffer. */
  pushRouteChange?(from: string, to: string, at?: number): void;
}

/** Dependencies needed only for the `structured_changes` command. */
export interface ChangeTrackingDeps {
  /* eslint-disable @typescript-eslint/no-explicit-any */
  createSnapshot: (...args: any[]) => any;
  createSnapshotManager: (...args: any[]) => any;
  analyzeStructuredChanges: (...args: any[]) => any;
  /* eslint-enable @typescript-eslint/no-explicit-any */
}

/**
 * Dispatch a snake_case change tracking command to the ChangeTracker.
 *
 * @returns The command result (varies per action).
 * @throws On missing required parameters (e.g. bookmark name).
 */
export async function handleChangeTrackingCommand(
  ct: ChangeTrackerLike,
  type: string,
  payload: Record<string, unknown>,
  deps: ChangeTrackingDeps,
): Promise<unknown> {
  switch (type) {
    case "save_bookmark": {
      if (typeof payload.name !== "string") {
        throw new Error("save_bookmark requires a 'name' parameter");
      }
      return ct.saveBookmark(payload.name);
    }
    case "get_bookmark": {
      if (typeof payload.name !== "string") {
        throw new Error("get_bookmark requires a 'name' parameter");
      }
      const bm = ct.getBookmark(payload.name);
      if (!bm) throw new Error(`Bookmark '${payload.name}' not found`);
      return bm;
    }
    case "delete_bookmark": {
      if (typeof payload.name !== "string") {
        throw new Error("delete_bookmark requires a 'name' parameter");
      }
      return { deleted: ct.deleteBookmark(payload.name) };
    }
    case "list_bookmarks":
      return ct.listBookmarks();
    case "diff_from_bookmark": {
      if (typeof payload.name !== "string") {
        throw new Error("diff_from_bookmark requires a 'name' parameter");
      }
      return ct.diffFromBookmark(payload.name);
    }
    case "execute_with_diff":
      return ct.executeWithDiff(payload);
    case "execute_batch_with_diff": {
      const operations = (payload.operations ?? []) as Array<Record<string, unknown>>;
      const results: unknown[] = [];
      for (const op of operations) {
        results.push(await ct.executeWithDiff(op));
      }
      return { results };
    }
    case "wait_for_change": {
      const wfcPayload = payload as {
        predicate: unknown;
        options?: unknown;
      };
      return ct.waitForChange(wfcPayload.predicate, wfcPayload.options);
    }
    case "categorize_last_diff":
      return ct.categorizeLastDiff();
    case "scoped_diff": {
      const sdPayload = payload as { scope: string; fromBookmark?: string };
      if (sdPayload.fromBookmark) {
        return ct.scopedDiffFromBookmark(sdPayload.fromBookmark, sdPayload.scope);
      }
      return null;
    }
    case "summarize_diff": {
      const sumBody = payload as {
        budget: number;
        includeIds?: boolean;
        includeCategory?: boolean;
        fromBookmark?: string;
      };
      const diff = sumBody.fromBookmark
        ? ct.diffFromBookmark(sumBody.fromBookmark)
        : (ct.categorizeLastDiff()?.diff ?? null);
      if (!diff) {
        return { summary: "No changes detected" };
      }
      return {
        summary: ct.summarizeDiff(diff, {
          budget: sumBody.budget,
          includeIds: sumBody.includeIds,
          includeCategory: sumBody.includeCategory,
        }),
      };
    }
    case "structured_changes": {
      const scPayload = payload as { fromBookmark?: string };
      if (scPayload?.fromBookmark) {
        const bm = ct.getBookmark(scPayload.fromBookmark);
        if (!bm) throw new Error(`Bookmark '${scPayload.fromBookmark}' not found`);
        const snap = deps.createSnapshot();
        const mgr = deps.createSnapshotManager({});
        const currentSemantic = mgr.createSnapshot({
          timestamp: Date.now(),
          // eslint-disable-next-line @typescript-eslint/no-explicit-any -- deps.createSnapshot() returns any
          elements: snap.elements.map((e: any) => ({
            id: e.id,
            type: e.type,
            label: e.label ?? "",
            actions: e.actions,
            state: e.state,
          })),
          components: [],
          workflows: [],
          activeRuns: [],
        });
        return deps.analyzeStructuredChanges(bm.snapshot, currentSemantic);
      }
      return { hasStructuredData: false, tableChanges: [], listChanges: [] };
    }
    case "enable_change_buffer":
      ct.enableBuffer();
      return { enabled: true };
    case "disable_change_buffer":
      ct.disableBuffer();
      return { enabled: false };
    case "drain_change_buffer":
      return ct.drainBuffer();
    case "get_change_buffer_size":
      return {
        size: ct.getBufferSize(),
        enabled: ct.isBufferEnabled(),
      };
    default:
      return undefined;
  }
}
