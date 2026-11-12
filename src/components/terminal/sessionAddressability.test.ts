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
 *     PER-ELEMENT advertised list, which the SDK registry of that day inferred
 *     for an `input` WITHOUT `sendKeys`. The components overrode the list until
 *     `@qontinui/ui-bridge` 0.46, whose one verb table advertises `sendKeys` on
 *     every kind; they now register no override, and these tests RENDER the two
 *     components inside the real `UIBridgeProvider` and read what the registry
 *     then serves. A future override that drops `sendKeys`, or an SDK that stops
 *     inferring it, fails here.
 *  2. Session cards and the Session Manager header carried no bridge id at all
 *     and had to be reached by a `page/evaluate` DOM scrape. The ids are
 *     author-stamped precisely because the auto-derived one is minted from
 *     element TEXT — and a card's text is the session title, which changes on
 *     every re-title.
 */

import { act, createElement, useEffect, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { UIBridgeProvider, useUIBridge, type UIBridgeRegistry } from "@qontinui/ui-bridge";

import servedActions from "@/lib/ui-bridge/servedActions.0.46.json";

import { CommandBar, COMMAND_BAR_INPUT_ID, COMMAND_BAR_INPUT_ELEMENT } from "./CommandBar";
import { sessionCardElementId } from "./SessionCard";
import {
  SEARCH_INPUT_ELEMENT,
  SESSION_MANAGER_HEADER_IDS,
  SessionManagerHeader,
  sessionManagerAccountFilterId,
} from "./SessionManagerHeader";
import { SESSION_MANAGER_TOGGLE_ID } from "./SessionManagerToggle";

// The command bar's IO-bound siblings and its terminal-session context are not
// what these tests observe; the bar's own input registration is.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve(null)) }));
vi.mock("./contexts/TerminalSessionContext", () => ({
  useTerminalSession: () => ({ tabs: [], activeId: null }),
}));
vi.mock("./SessionManagerToggle", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./SessionManagerToggle")>()),
  SessionManagerToggle: () => null,
}));
vi.mock("./TerminalWorkModeToggle", () => ({ TerminalWorkModeToggle: () => null }));
vi.mock("./SpawnTenantPicker", () => ({ SpawnTenantPicker: () => null }));

let container: HTMLDivElement;
let root: Root;
let registry: UIBridgeRegistry | null = null;

function CaptureRegistry() {
  const bridgeRegistry = useUIBridge().registry;
  useEffect(() => {
    registry = bridgeRegistry;
  }, [bridgeRegistry]);
  return null;
}

/** Render `child` inside the real SDK provider and return what `id` serves. */
function servedFor(child: ReactNode, id: string): readonly string[] {
  act(() => {
    root.render(
      createElement(UIBridgeProvider, {
        features: {},
        children: [createElement(CaptureRegistry, { key: "probe" }), child],
      }),
    );
  });
  const element = registry?.getElement(id);
  expect(element, `${id} is registered`).toBeDefined();
  return element!.actions;
}

beforeEach(() => {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  registry = null;
});

const noop = () => {};

/** The verbs the old override listed: every one must still be served. */
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
  it("the components register no actions override and no custom sendKeys", () => {
    // An override REPLACES the inferred list, so one that forgets a verb
    // silently un-advertises a working action. And `TerminalInstance.tsx`
    // registers a CUSTOM `sendKeys` on the terminal PANE taking a plain
    // string, which would shadow the built-in (`keys: [{key, modifiers?}]`).
    for (const descriptor of [COMMAND_BAR_INPUT_ELEMENT, SEARCH_INPUT_ELEMENT]) {
      expect("actions" in descriptor).toBe(false);
      expect("customActions" in descriptor).toBe(false);
    }
  });

  it("the rendered command bar input serves the 0.46 list, sendKeys and the old override's verbs included", () => {
    const actions = servedFor(createElement(CommandBar), COMMAND_BAR_INPUT_ID);
    for (const verb of FORMER_OVERRIDE) expect(actions).toContain(verb);
    // Its structure adds what it performs: a text field (`setValue`,
    // `autocomplete`), `role="combobox"` (`select`), `aria-expanded` (`toggle`).
    // The same list the Rust vocabulary tests read (`action_vocabulary.rs`).
    expect(actions).toEqual(servedActions.commandBarInput);
  });

  it("the rendered session search box serves sendKeys and every verb the old override listed", () => {
    const actions = servedFor(
      createElement(SessionManagerHeader, {
        loading: false,
        searchQuery: "",
        onSearchChange: noop,
        statusFilter: "all",
        onStatusFilterChange: noop,
        accountFilter: "all",
        onAccountFilterChange: noop,
        groupBy: "status",
        onGroupByChange: noop,
        sortBy: "recent",
        onSortByChange: noop,
        accountUsage: [],
        accounts: [],
        onRefresh: noop,
        frozenCount: 0,
        needsInputCount: 0,
        activeCount: 0,
        totalCount: 0,
      }),
      SESSION_MANAGER_HEADER_IDS.search,
    );
    for (const verb of FORMER_OVERRIDE) expect(actions).toContain(verb);
    expect(actions).toEqual(servedActions.autoRegisteredTextInput);
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
