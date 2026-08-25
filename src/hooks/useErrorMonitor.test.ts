import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, it, expect } from "vitest";
import { filterListKey } from "./useErrorMonitor";
import type { ErrorSeverity, ErrorStatus } from "../types/errorMonitor";

const SOURCE = readFileSync(
  fileURLToPath(new URL("./useErrorMonitor.ts", import.meta.url)),
  "utf8",
);

/**
 * The body of `useErrorEvents`, up to the auto-refresh effect. Slicing keeps
 * the assertions below from accidentally matching the other three hooks in this
 * file, which legitimately still read their options out of a ref (none of them
 * takes a user-toggled filter).
 */
function errorEventsHookSource(): string {
  const start = SOURCE.indexOf("export function useErrorEvents");
  expect(start).toBeGreaterThan(-1);
  const end = SOURCE.indexOf("export function useErrorSummary");
  expect(end).toBeGreaterThan(start);
  return SOURCE.slice(start, end);
}

/** The dependency array literal of `useErrorEvents`' `fetchErrors` callback. */
function fetchErrorsDeps(): string {
  const body = errorEventsHookSource();
  const start = body.indexOf("const fetchErrors = useCallback(");
  expect(start).toBeGreaterThan(-1);
  const match = /\}\s*,\s*\[([^\]]*)\]\s*\)\s*;/.exec(body.slice(start));
  expect(match, "fetchErrors must close with a dependency array literal").not.toBeNull();
  return (match as RegExpExecArray)[1];
}

describe("useErrorEvents filter reactivity (iter 19, item C)", () => {
  /**
   * The defect: `fetchErrors` read every filter out of `optionsRef` and
   * declared `[]` as its dependencies. Its identity therefore never changed, no
   * effect re-ran, and clicking a filter pill left the OLD rows on screen —
   * under the NEW pill state — until the 30s auto-refresh tick came round.
   *
   * These filters are applied server-side, so a stale fetch is a wrong list,
   * not merely a late one.
   */
  it("fetchErrors depends on every server-side filter", () => {
    const deps = fetchErrorsDeps();
    for (const dep of ["taskRunId", "logSourceName", "severitiesKey", "statusesKey", "limit"]) {
      expect(deps, `fetchErrors must re-create when ${dep} changes`).toContain(dep);
    }
  });

  /**
   * And it must read those filters from the same values it watched. Reading
   * `optionsRef.current` inside the callback while depending on the keys would
   * type-check, pass the assertion above, and still be able to fetch with
   * filters the dependency list never saw.
   */
  it("fetchErrors builds its query from the watched values, not from a ref", () => {
    const body = errorEventsHookSource();
    const start = body.indexOf("const fetchErrors = useCallback(");
    const end = body.indexOf("[taskRunId", start);
    const callback = body.slice(start, end > start ? end : undefined);
    expect(
      callback.includes("optionsRef"),
      "useErrorEvents must not read its filters out of a ref — that is what made the " +
        "dependency list unable to see a pill toggle",
    ).toBe(false);
  });
});

describe("filterListKey", () => {
  it("distinguishes filter sets that must produce different fetches", () => {
    const withoutRecurring: ErrorStatus[] = ["new", "acknowledged"];
    const withRecurring: ErrorStatus[] = ["new", "acknowledged", "recurring"];

    expect(filterListKey(withRecurring)).not.toBe(filterListKey(withoutRecurring));

    const severities: ErrorSeverity[] = ["critical"];
    expect(filterListKey(severities)).not.toBe(filterListKey(["critical", "warning"]));
  });

  /**
   * The other failure direction. Callers pass a NEW array literal every render;
   * if equal contents did not key equal, every render would refetch — a 100ms
   * query loop instead of a 30s one.
   */
  it("keys equal contents identically across fresh array literals", () => {
    expect(filterListKey(["new", "recurring"])).toBe(filterListKey(["new", "recurring"]));
    expect(filterListKey(undefined)).toBe(filterListKey([]));
  });

  it("round-trips through split, so the rebuilt query matches the original filter", () => {
    const statuses: ErrorStatus[] = ["new", "recurring", "acknowledged", "in_progress", "promoted"];
    expect(filterListKey(statuses).split(",")).toEqual(statuses);
  });
});
