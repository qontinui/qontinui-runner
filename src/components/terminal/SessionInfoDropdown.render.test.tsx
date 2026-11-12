// @vitest-environment jsdom
/**
 * The REAL `SessionInfoDropdown`, rendered and opened (plan
 * `2026-11-12-session-info-row-title-is-unreachable-through-the-ui-bridge`,
 * Phase 2).
 *
 * The Tenant row's title is the only place the full acting id, every bound
 * tenant and coord's session-row leg are all shown. `useSessionInfo.test.ts`
 * pins the title's CONTENT through `tenantRow`, but that test cannot see
 * whether `InfoRow` puts that title on the value span, and no other vitest
 * suite opens the panel: `SessionInfoDropdown.test.ts` reads the component's
 * source text, `ZoneGrid.tmux.test.tsx` and `zone-grid/zoneCells.test.tsx`
 * mock the dropdown away, and `zone-grid/zoneLabelRow.test.tsx` reads only its
 * closed trigger. The operator-run `scripts/uibridge-session-info.test.mjs`
 * opens it on a live runner but never reads `title`.
 * So this file renders the dropdown, opens it, and reads the `title` attribute
 * off the span that `GET /ui-bridge/control/element/<row id>/tree?depth=1`
 * serves as the child carrying `data-session-info-overflow="truncate"`.
 *
 * Mocks follow `TenantBadge.render.test.tsx` and
 * `zone-grid/zoneLabelRow.test.tsx`. `useUIElement` is stubbed, so the test
 * does not depend on the SDK build in the local `node_modules`.
 */

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  BAUHAUS,
  body,
  display,
  MERYTS,
  PIZZERIA,
  QONTINUI,
  tenancyWith,
} from "./useSessionInfo.testkit";

// Every IPC read stays pending; the hook is mocked below, so nothing reads.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => new Promise(() => {})) }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
vi.mock("@qontinui/ui-bridge", () => ({ useUIElement: () => ({ ref: { current: null } }) }));

// The hook's real return shape (`SessionInfoState & { refresh }`). Opening the
// panel calls `state.refresh()`, so a mock without `refresh` throws on click.
// vitest types a string-path factory's result loosely, so the return type is
// annotated here to make `tsc --noEmit` hold the stub to the real hook.
const hook = vi.hoisted(() => ({ refresh: vi.fn() }));
vi.mock("./useSessionInfo", async (orig) => ({
  ...(await orig<typeof import("./useSessionInfo")>()),
  useSessionInfo: (): ReturnType<typeof import("./useSessionInfo").useSessionInfo> => ({
    status: "ok",
    reason: null,
    // The tenancy `useSessionInfo.test.ts` uses for the row-title test.
    body: body({ tenancy: tenancyWith(display({ tenantSlug: "qontinui" })) }),
    refresh: hook.refresh,
  }),
}));

import { SessionInfoDropdown } from "./SessionInfoDropdown";

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  hook.refresh.mockClear();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

/** Render zone 0 and open its panel through the trigger, as a user would. */
function renderOpenPanel(): void {
  act(() => {
    root.render(
      createElement(SessionInfoDropdown, {
        claudeSessionId: "8b45800b-4b65-4c15-afd4-d0feaa0cb0d4",
        zoneIndex: 0,
      }),
    );
  });
  const trigger = container.querySelector<HTMLButtonElement>(
    '[data-testid="terminal-session-info-trigger-0"]',
  );
  expect(trigger).not.toBeNull();
  act(() => {
    trigger?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  expect(trigger?.getAttribute("aria-expanded")).toBe("true");
  expect(hook.refresh).toHaveBeenCalledTimes(1);
}

/** The value span of one row: the child `/tree?depth=1` serves with the title. */
function valueSpan(rowId: string): HTMLElement {
  const span = container.querySelector<HTMLElement>(
    `[data-testid="${rowId}"] [data-session-info-overflow="truncate"]`,
  );
  if (span === null) throw new Error(`no value span under ${rowId}`);
  return span;
}

describe("SessionInfoDropdown row titles on the value span", () => {
  it("puts the Tenant row's full title — acting id, bound set, coord row — on its value span", () => {
    renderOpenPanel();
    expect(valueSpan("terminal-session-info-tenant-0").getAttribute("title")).toBe(
      [
        `Acting tenant: ${QONTINUI} (qontinui) — source credential`,
        `Bound tenants (4): ${MERYTS}, ${QONTINUI}, ${BAUHAUS}, ${PIZZERIA}`,
        `Coord session row: ${QONTINUI} (as registered by this runner)`,
      ].join("\n"),
    );
  });

  it("falls back to the row's own text for a row that has no separate title", () => {
    renderOpenPanel();
    const span = valueSpan("terminal-session-info-terminal-id-0");
    expect(span.textContent).toBe("term-1");
    expect(span.getAttribute("title")).toBe(span.textContent);
  });

  it("falls back to the row's display, never its raw value", () => {
    // The Terminal id row's display equals its value, so the test above cannot
    // tell the two apart. The Recorded zone row's value is the 0-based index
    // (3) and its display is "Zone 4", so a fallback to `row.value` fails here.
    renderOpenPanel();
    const span = valueSpan("terminal-session-info-recorded-zone-0");
    expect(span.textContent).toBe("Zone 4");
    expect(span.getAttribute("title")).toBe("Zone 4");
  });
});
