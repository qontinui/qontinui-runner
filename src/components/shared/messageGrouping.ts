/**
 * Source-grouping for AI conversation entries.
 *
 * Split out of `AiMessageDisplay.tsx` so it is a leaf module (no React, no
 * lucide, no design-system imports) and therefore unit-testable under the
 * runner's `environment: "node"` vitest config.
 *
 * Beyond the original one-shot {@link groupEntriesBySource}, this module adds
 * {@link createIncrementalSourceGrouper}: a stateful grouper that appends the
 * newly arrived entries onto the previous result instead of re-grouping the
 * whole conversation on every streamed line (plan
 * `2026-07-28-runner-many-sessions-performance.md` §A5c).
 *
 * Two identity guarantees the render path depends on:
 *
 *  - When the entry list is unchanged, `group()` returns the SAME array
 *    instance, so `React.memo` on the display component holds.
 *  - When entries are appended, only the groups that actually changed are
 *    replaced with new objects; untouched groups keep their identity, so
 *    `React.memo` on the per-message blocks holds for the whole backlog and
 *    only the tail message re-renders.
 */

/**
 * Entry type for AI messages.
 * Compatible with both AiOutputLine and AiOutputEntry.
 */
export interface AiMessageEntry {
  id: string;
  timestamp: number;
  line: string;
  source: string; // "prompt", "claude", "response", "user_hint", "user_message", or orchestrator sources
}

/**
 * A group of consecutive entries from the same source.
 */
export interface MessageGroup {
  source: string;
  entries: AiMessageEntry[];
  timestamp: number;
  combinedText: string;
}

/** Stable empty result so callers never allocate a fresh `[]` per render. */
export const EMPTY_MESSAGE_GROUPS: MessageGroup[] = [];

/** `"response"` is a display alias for `"claude"`. */
function normalizeSource(source: string): string {
  return source === "response" ? "claude" : source;
}

/**
 * Groups consecutive entries by their source.
 * This is the core grouping logic used by both AiOutputTab and AiConversationWidget.
 */
export function groupEntriesBySource<T extends AiMessageEntry>(
  entries: readonly T[],
): MessageGroup[] {
  if (entries.length === 0) return [];

  const groups: MessageGroup[] = [];
  let currentGroup: MessageGroup | null = null;

  for (const entry of entries) {
    const source = normalizeSource(entry.source);

    if (!currentGroup || currentGroup.source !== source) {
      if (currentGroup) {
        groups.push(currentGroup);
      }
      currentGroup = {
        source,
        entries: [entry],
        timestamp: entry.timestamp,
        combinedText: entry.line,
      };
    } else {
      currentGroup.entries.push(entry);
      currentGroup.combinedText += "\n" + entry.line;
    }
  }

  if (currentGroup) {
    groups.push(currentGroup);
  }

  return groups;
}

/** A grouper that reuses its previous result when the entries only grew. */
export interface IncrementalSourceGrouper {
  /**
   * Group `entries`. Returns the previous array instance verbatim when nothing
   * changed; otherwise a new array in which unchanged groups keep their
   * identity.
   */
  group(entries: readonly AiMessageEntry[]): MessageGroup[];
  /** Number of entries folded into the current result (test/diagnostic hook). */
  readonly processedCount: number;
  /** Times a full re-group was needed (test/diagnostic hook). */
  readonly fullRegroupCount: number;
}

/**
 * Create a grouper that appends incrementally.
 *
 * The fast path is taken when the incoming array is a strict extension of the
 * one previously grouped — same length-or-longer, and the same entry ids at the
 * head and at the previous tail. Anything else (a different conversation, a
 * trimmed log, an edit) falls back to a full re-group, so the output is always
 * identical to {@link groupEntriesBySource}.
 */
export function createIncrementalSourceGrouper(): IncrementalSourceGrouper {
  let groups: MessageGroup[] = EMPTY_MESSAGE_GROUPS;
  let lastEntries: readonly AiMessageEntry[] | null = null;
  let processed = 0;
  let headId: string | null = null;
  let tailId: string | null = null;
  let fullRegroups = 0;

  function remember(entries: readonly AiMessageEntry[], next: MessageGroup[]): MessageGroup[] {
    groups = next;
    lastEntries = entries;
    processed = entries.length;
    headId = entries.length > 0 ? entries[0].id : null;
    tailId = entries.length > 0 ? entries[entries.length - 1].id : null;
    return groups;
  }

  function canAppend(entries: readonly AiMessageEntry[]): boolean {
    if (processed === 0) return false;
    if (entries.length < processed) return false;
    if (entries[0]?.id !== headId) return false;
    return entries[processed - 1]?.id === tailId;
  }

  return {
    group(entries: readonly AiMessageEntry[]): MessageGroup[] {
      if (entries === lastEntries) return groups;

      if (entries.length === 0) {
        return remember(entries, EMPTY_MESSAGE_GROUPS);
      }

      if (!canAppend(entries)) {
        fullRegroups++;
        return remember(entries, groupEntriesBySource(entries));
      }

      if (entries.length === processed) {
        // Same entries in a differently-identified array: reuse everything so
        // downstream memoisation sees no change at all.
        lastEntries = entries;
        return groups;
      }

      // Copy-on-append: new array identity, unchanged groups keep theirs.
      const next = groups.slice();
      let tail = next.length > 0 ? next[next.length - 1] : null;

      for (let i = processed; i < entries.length; i++) {
        const entry = entries[i];
        const source = normalizeSource(entry.source);

        if (tail && tail.source === source) {
          // Replace the tail group with an extended clone so memoised children
          // of the *unchanged* groups keep their identity while this one updates.
          tail = {
            source: tail.source,
            entries: [...tail.entries, entry],
            timestamp: tail.timestamp,
            combinedText: tail.combinedText + "\n" + entry.line,
          };
          next[next.length - 1] = tail;
        } else {
          tail = {
            source,
            entries: [entry],
            timestamp: entry.timestamp,
            combinedText: entry.line,
          };
          next.push(tail);
        }
      }

      return remember(entries, next);
    },
    get processedCount() {
      return processed;
    },
    get fullRegroupCount() {
      return fullRegroups;
    },
  };
}
