/**
 * Tests for the incremental source grouper (plan
 * `2026-07-28-runner-many-sessions-performance.md` §A5c).
 *
 * Two properties matter:
 *   1. **Equivalence** — incremental grouping must produce exactly what the
 *      one-shot `groupEntriesBySource` produces, for every append pattern.
 *   2. **Identity stability** — unchanged groups must keep their object
 *      identity and an unchanged entry list must return the same array, or the
 *      `React.memo` wrappers on the message blocks are worthless.
 */

import { describe, it, expect } from "vitest";

import {
  createIncrementalSourceGrouper,
  groupEntriesBySource,
  type AiMessageEntry,
} from "./messageGrouping";

let seq = 0;
function entry(source: string, line: string, timestamp = 1000): AiMessageEntry {
  seq += 1;
  return { id: `e${seq}`, timestamp, line, source };
}

describe("groupEntriesBySource", () => {
  it("groups consecutive entries and normalises 'response' to 'claude'", () => {
    const entries = [
      entry("prompt", "hi"),
      entry("claude", "a"),
      entry("response", "b"),
      entry("user_hint", "note"),
      entry("claude", "c"),
    ];
    const groups = groupEntriesBySource(entries);

    expect(groups.map((g) => g.source)).toEqual(["prompt", "claude", "user_hint", "claude"]);
    expect(groups[1].combinedText).toBe("a\nb");
    expect(groups[1].entries).toHaveLength(2);
  });

  it("returns [] for no entries", () => {
    expect(groupEntriesBySource([])).toEqual([]);
  });
});

describe("createIncrementalSourceGrouper", () => {
  it("matches the one-shot grouper after every append", () => {
    const grouper = createIncrementalSourceGrouper();
    const entries: AiMessageEntry[] = [];
    const sources = ["prompt", "claude", "claude", "claude", "user_hint", "claude", "response"];

    for (let i = 0; i < 60; i++) {
      entries.push(entry(sources[i % sources.length], `line ${i}`));
      const incremental = grouper.group([...entries]);
      expect(incremental).toEqual(groupEntriesBySource(entries));
    }
  });

  it("appends without re-grouping the backlog", () => {
    const grouper = createIncrementalSourceGrouper();
    const entries = [entry("prompt", "p"), entry("claude", "a")];
    grouper.group([...entries]);
    const regroupsAfterFirst = grouper.fullRegroupCount;

    for (let i = 0; i < 100; i++) {
      entries.push(entry("claude", `chunk ${i}`));
      grouper.group([...entries]);
    }

    expect(grouper.fullRegroupCount).toBe(regroupsAfterFirst);
    expect(grouper.processedCount).toBe(entries.length);
  });

  it("keeps unchanged groups' identity while replacing only the tail group", () => {
    const grouper = createIncrementalSourceGrouper();
    const entries = [entry("prompt", "p"), entry("claude", "a")];
    const first = grouper.group([...entries]);

    entries.push(entry("claude", "b"));
    const second = grouper.group([...entries]);

    expect(second).not.toBe(first); // content changed -> new array identity
    expect(second[0]).toBe(first[0]); // untouched group keeps identity
    expect(second[1]).not.toBe(first[1]); // tail group is a fresh object
    expect(second[1].combinedText).toBe("a\nb");
    // The previously returned group object must not have been mutated.
    expect(first[1].combinedText).toBe("a");
  });

  it("returns the SAME array when the entry list has not changed", () => {
    const grouper = createIncrementalSourceGrouper();
    const entries = [entry("prompt", "p"), entry("claude", "a")];

    const first = grouper.group(entries);
    expect(grouper.group(entries)).toBe(first); // same array instance in
    expect(grouper.group([...entries])).toBe(first); // fresh array, same content
  });

  it("starting a new group on a source change does not touch earlier groups", () => {
    const grouper = createIncrementalSourceGrouper();
    const entries = [entry("claude", "a")];
    const first = grouper.group([...entries]);

    entries.push(entry("prompt", "q"));
    const second = grouper.group([...entries]);

    expect(second[0]).toBe(first[0]);
    expect(second).toHaveLength(2);
    expect(second[1].source).toBe("prompt");
  });

  it("falls back to a full re-group when the conversation is replaced", () => {
    const grouper = createIncrementalSourceGrouper();
    const a = [entry("claude", "a"), entry("claude", "b")];
    grouper.group(a);
    const before = grouper.fullRegroupCount;

    const b = [entry("prompt", "other")];
    const groups = grouper.group(b);

    expect(grouper.fullRegroupCount).toBe(before + 1);
    expect(groups).toEqual(groupEntriesBySource(b));
  });

  it("falls back to a full re-group when the head is trimmed (log store cap)", () => {
    const grouper = createIncrementalSourceGrouper();
    const entries = [entry("claude", "a"), entry("claude", "b"), entry("claude", "c")];
    grouper.group([...entries]);
    const before = grouper.fullRegroupCount;

    const trimmed = entries.slice(1);
    const groups = grouper.group(trimmed);

    expect(grouper.fullRegroupCount).toBe(before + 1);
    expect(groups).toEqual(groupEntriesBySource(trimmed));
  });

  it("handles going empty and back", () => {
    const grouper = createIncrementalSourceGrouper();
    grouper.group([entry("claude", "a")]);
    expect(grouper.group([])).toEqual([]);

    const restored = [entry("prompt", "p")];
    expect(grouper.group(restored)).toEqual(groupEntriesBySource(restored));
  });
});
