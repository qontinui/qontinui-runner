/**
 * Pure-helper test for the PlanTaskBoard pane error rendering.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom), so the
 * board can't be rendered here — the classification the fix turns on is
 * extracted as a pure predicate for exactly that reason.
 */

import { describe, expect, it, vi } from "vitest";

// The module pulls in Tauri event APIs at import time; stub them so the
// pure export is reachable in a node environment.
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

import { isUnregisteredCommandError } from "./PlanTaskBoard";

describe("isUnregisteredCommandError", () => {
  it.each([
    "Command list_plans_filtered not found",
    "command get_plan_tasks not found",
    "list_plans not allowed",
    "unknown command: productivity_x",
  ])("recognises the unregistered-command shape %j", (message) => {
    expect(isUnregisteredCommandError(message)).toBe(true);
  });

  it.each([
    'db error',
    'error returned from database: relation "coord.plans" does not exist',
    "PG pool error: timed out waiting for connection",
    "list_plans_filtered timed out after 10000ms",
  ])("does NOT claim %j is a registration problem — THE DEFECT", (message) => {
    // Every pane hardcoded "the backend command may not be registered yet"
    // for ANY failure and threw the real message away, so a live command
    // failing on a missing table sent the operator hunting a build problem
    // that did not exist.
    expect(isUnregisteredCommandError(message)).toBe(false);
  });
});
