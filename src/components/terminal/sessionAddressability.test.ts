/**
 * Regression tests for the terminal-page UI-Bridge addressability contract
 * (plan `2026-07-19-session-titles-ansi-and-ui-bridge-friction`, Tasks B + C).
 *
 * Two defects are pinned here:
 *
 *  1. Driving the command bar failed with `ACTION_NOT_SUPPORTED` —
 *     "Action 'sendKeys' is not supported by element …" — even though BOTH
 *     global whitelists carry `sendKeys`. The rejection came from the
 *     PER-ELEMENT advertised list, which the registry infers for an `input`
 *     WITHOUT `sendKeys`. Any future edit that drops `sendKeys` from a text
 *     input's advertised set silently restores that failure, so it is asserted
 *     rather than left to review.
 *  2. Session cards and the Session Manager header carried no bridge id at all
 *     and had to be reached by a `page/evaluate` DOM scrape. The ids are
 *     author-stamped precisely because the auto-derived one is minted from
 *     element TEXT — and a card's text is the session title, which changes on
 *     every re-title.
 *
 * The `useUIElement` calls themselves need a DOM; these specs are pure values,
 * so they are asserted directly (vitest `node` env — same precedent as
 * `ZoneLabel.test.ts`).
 */

import { describe, it, expect } from "vitest";

import { COMMAND_BAR_INPUT_ID, COMMAND_BAR_INPUT_ACTIONS } from "./CommandBar";
import { sessionCardElementId } from "./SessionCard";
import {
  SEARCH_INPUT_ACTIONS,
  SESSION_MANAGER_HEADER_IDS,
  sessionManagerAccountFilterId,
} from "./SessionManagerHeader";
import { SESSION_MANAGER_TOGGLE_ID } from "./SessionManagerToggle";

describe("text inputs advertise sendKeys", () => {
  it("the command bar input advertises sendKeys", () => {
    expect(COMMAND_BAR_INPUT_ACTIONS).toContain("sendKeys");
  });

  it("the session search box advertises sendKeys", () => {
    expect(SEARCH_INPUT_ACTIONS).toContain("sendKeys");
  });

  it("advertises sendKeys ALONGSIDE the inferred input actions, not instead of them", () => {
    // `inferActions('input')` in the SDK registry. Overriding `actions` REPLACES
    // the inferred list, so an override that forgets one of these silently
    // un-advertises a working action.
    for (const inferred of [
      "focus",
      "blur",
      "hover",
      "scroll",
      "scrollIntoView",
      "click",
      "hoverClick",
      "type",
      "clear",
    ]) {
      expect(COMMAND_BAR_INPUT_ACTIONS).toContain(inferred);
      expect(SEARCH_INPUT_ACTIONS).toContain(inferred);
    }
  });

  it("advertises no custom action name — sendKeys keeps the SDK descriptor-array contract", () => {
    // `TerminalInstance.tsx` registers a per-element CUSTOM `sendKeys` on the
    // terminal PANE taking a plain string, and a registered custom action wins
    // over the same-named built-in on that element. These two inputs must stay
    // on the built-in (`keys: [{key, modifiers?}]`), so every entry has to be a
    // member of the SDK's `StandardAction` union — a name outside it would mean
    // a third meaning had been introduced.
    const STANDARD_ACTIONS = new Set([
      "click",
      "hoverClick",
      "doubleClick",
      "rightClick",
      "middleClick",
      "type",
      "sendKeys",
      "clear",
      "select",
      "focus",
      "blur",
      "hover",
      "scroll",
      "scrollIntoView",
      "check",
      "uncheck",
      "toggle",
      "setValue",
      "drag",
      "submit",
      "reset",
      "autocomplete",
    ]);
    for (const action of [...COMMAND_BAR_INPUT_ACTIONS, ...SEARCH_INPUT_ACTIONS]) {
      expect(STANDARD_ACTIONS.has(action)).toBe(true);
    }
  });
});

describe("author-stamped control ids", () => {
  it("session card ids are per-session and stable against a re-title", () => {
    const id = sessionCardElementId("5f92f974-e0ab-4fa7-b8b3-0c023145b659");
    expect(id).toBe("terminal.session-card-5f92f974-e0ab-4fa7-b8b3-0c023145b659");
    // Derived from the session id ALONE — nothing about the card's rendered
    // text can move it.
    expect(id).toBe(sessionCardElementId("5f92f974-e0ab-4fa7-b8b3-0c023145b659"));
  });

  it("two sessions never collide in the registry", () => {
    expect(sessionCardElementId("aaa")).not.toBe(sessionCardElementId("bbb"));
  });

  it("every id follows the terminal.* convention the toggle established", () => {
    expect(SESSION_MANAGER_TOGGLE_ID).toBe("terminal.session-manager-toggle");
    for (const id of [
      COMMAND_BAR_INPUT_ID,
      sessionCardElementId("x"),
      sessionManagerAccountFilterId("gmail"),
      ...Object.values(SESSION_MANAGER_HEADER_IDS),
    ]) {
      expect(id.startsWith("terminal.")).toBe(true);
    }
  });

  it("the header ids are all distinct", () => {
    const ids = Object.values(SESSION_MANAGER_HEADER_IDS);
    expect(new Set(ids).size).toBe(ids.length);
  });
});
