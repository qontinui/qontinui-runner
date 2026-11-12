// @vitest-environment jsdom
/**
 * Regression tests for the terminal-page UI-Bridge addressability contract
 * (plan `2026-07-19-session-titles-ansi-and-ui-bridge-friction`, Tasks B + C).
 *
 * Two defects are pinned here:
 *
 *  1. Driving the command bar failed with `ACTION_NOT_SUPPORTED` —
 *     "Action 'sendKeys' is not supported by element …" — even though BOTH
 *     global whitelists carry `sendKeys`. The rejection came from the
 *     PER-ELEMENT advertised list, which the pre-0.46 registry inferred for an
 *     `input` WITHOUT `sendKeys`. The components used to override the list;
 *     since `@qontinui/ui-bridge` 0.46 the SDK's one verb table advertises
 *     `sendKeys` on every kind, so they register no override and these tests
 *     register their EXACT descriptors through the real SDK registry and read
 *     what it serves. A future override that drops `sendKeys`, or an SDK that
 *     stops inferring it, fails here.
 *  2. Session cards and the Session Manager header carried no bridge id at all
 *     and had to be reached by a `page/evaluate` DOM scrape. The ids are
 *     author-stamped precisely because the auto-derived one is minted from
 *     element TEXT — and a card's text is the session title, which changes on
 *     every re-title.
 */

import { describe, it, expect } from "vitest";
import { UIBridgeRegistry } from "@qontinui/ui-bridge";

import { COMMAND_BAR_INPUT_ID, COMMAND_BAR_INPUT_ELEMENT } from "./CommandBar";
import { sessionCardElementId } from "./SessionCard";
import {
  SEARCH_INPUT_ELEMENT,
  SESSION_MANAGER_HEADER_IDS,
  sessionManagerAccountFilterId,
} from "./SessionManagerHeader";
import { SESSION_MANAGER_TOGGLE_ID } from "./SessionManagerToggle";

/**
 * The `<input>` each component renders, with the attributes that decide its
 * verbs (`CommandBar.tsx`: a text input carrying `role="combobox"` and
 * `aria-expanded`; `SessionManagerHeader.tsx`: `type="text"`).
 */
function commandBarInput(): HTMLInputElement {
  const input = document.createElement("input");
  input.setAttribute("data-ui-bridge-id", COMMAND_BAR_INPUT_ID);
  input.setAttribute("aria-label", "Terminal command bar");
  input.setAttribute("role", "combobox");
  input.setAttribute("aria-expanded", "false");
  input.setAttribute("aria-autocomplete", "list");
  document.body.appendChild(input);
  return input;
}

function searchInput(): HTMLInputElement {
  const input = document.createElement("input");
  input.setAttribute("data-ui-bridge-id", SESSION_MANAGER_HEADER_IDS.search);
  input.setAttribute("aria-label", "Search sessions");
  input.type = "text";
  document.body.appendChild(input);
  return input;
}

/** What the registry advertises for `descriptor` registered on `element`. */
function advertised(
  descriptor: typeof COMMAND_BAR_INPUT_ELEMENT | typeof SEARCH_INPUT_ELEMENT,
  element: HTMLElement,
): readonly string[] {
  return new UIBridgeRegistry().registerElement(descriptor.id, element, { ...descriptor }).actions;
}

/** The verbs the pre-0.46 override listed: every one must still be served. */
const FORMER_OVERRIDE = [
  "focus",
  "blur",
  "hover",
  "scroll",
  "scrollIntoView",
  "click",
  "hoverClick",
  "type",
  "clear",
  "sendKeys",
];

describe("text inputs advertise sendKeys", () => {
  it("the components register no actions override", () => {
    // An override REPLACES the inferred list, so one that forgets a verb
    // silently un-advertises a working action. 0.46's inferred list already
    // carries everything the old override restated.
    expect("actions" in COMMAND_BAR_INPUT_ELEMENT).toBe(false);
    expect("actions" in SEARCH_INPUT_ELEMENT).toBe(false);
  });

  it("the command bar input advertises sendKeys and every verb the old override listed", () => {
    const actions = advertised(COMMAND_BAR_INPUT_ELEMENT, commandBarInput());
    for (const verb of FORMER_OVERRIDE) expect(actions).toContain(verb);
    // Its structure adds what it performs: a text field (`setValue`,
    // `autocomplete`), `role="combobox"` (`select`), `aria-expanded` (`toggle`).
    for (const verb of ["setValue", "autocomplete", "select", "toggle"]) {
      expect(actions).toContain(verb);
    }
  });

  it("the session search box advertises sendKeys and every verb the old override listed", () => {
    const actions = advertised(SEARCH_INPUT_ELEMENT, searchInput());
    for (const verb of FORMER_OVERRIDE) expect(actions).toContain(verb);
    expect(actions).toContain("setValue");
  });

  it("sendKeys stays the SDK built-in — no custom action shadows it", () => {
    // `TerminalInstance.tsx` registers a per-element CUSTOM `sendKeys` on the
    // terminal PANE taking a plain string, and a registered custom action wins
    // over the same-named built-in on that element. These two inputs must stay
    // on the built-in (`keys: [{key, modifiers?}]`).
    expect("customActions" in COMMAND_BAR_INPUT_ELEMENT).toBe(false);
    expect("customActions" in SEARCH_INPUT_ELEMENT).toBe(false);
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
