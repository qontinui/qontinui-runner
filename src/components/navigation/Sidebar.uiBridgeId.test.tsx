/**
 * Addressability contract for the sidebar's nav buttons.
 *
 * THE DEFECT: neither `NavItem` nor `FlyoutItem` stamped a
 * `data-ui-bridge-id`, so `AutoRegisterProvider` (`App.tsx`,
 * `idStrategy="prefer-existing"`) derived one through `generateSemanticId`,
 * whose base id embeds `getSiblingIndex(element)` — a POSITIONAL index over the
 * parent's same-tag children. The same button therefore alternated between
 * `button-projects` and `button-projects-0` across re-renders, `button-settings`
 * and `button-help` collided on `-0`/`-1` with `button-projects`/`button-terminal`
 * under a different parent, and every recorded automation script was
 * unreplayable without a `POST /control/discover` between each click.
 *
 * The fix stamps the id `useNavigationItem` ALREADY publishes for the same
 * control — `nav:<item.id>` — rather than minting a second vocabulary.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom, no
 * `@testing-library/react` — see `StewardControl.test.tsx`), so the render
 * assertions go through `react-dom/server` exactly as
 * `StreamingMessageView.test.tsx` does. Initial render is all these cases need.
 */

import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { Play } from "lucide-react";

import { FlyoutItem, NavItem, type ResolvedNavigationItem } from "./Sidebar";

const item = (overrides: Partial<ResolvedNavigationItem> = {}): ResolvedNavigationItem => ({
  id: "projects",
  label: "Projects",
  icon: Play,
  ...overrides,
});

/** Pull one attribute's value off the rendered `<button>`. */
function attr(html: string, name: string): string | null {
  const m = html.match(new RegExp(`${name}="([^"]*)"`));
  return m ? m[1] : null;
}

const renderNavItem = (i: ResolvedNavigationItem) =>
  renderToStaticMarkup(
    <NavItem
      item={i}
      isActive={false}
      collapsed={false}
      onClick={() => {}}
      onKeyDown={() => {}}
      tabIndex={0}
      dataNavItem={i.id}
    />,
  );

const renderFlyoutItem = (i: ResolvedNavigationItem) =>
  renderToStaticMarkup(<FlyoutItem item={i} isActive={false} onClick={() => {}} index={0} />);

describe("NavItem addressability", () => {
  it("stamps the `nav:<id>` the navigation package already publishes", () => {
    expect(attr(renderNavItem(item()), "data-ui-bridge-id")).toBe("nav:projects");
  });

  it("derives the id from the item, not from DOM position", () => {
    // Two items rendered from the same call site get DIFFERENT ids — under the
    // old auto-derivation both would have been minted from a sibling index.
    expect(
      attr(renderNavItem(item({ id: "settings", label: "Settings" })), "data-ui-bridge-id"),
    ).toBe("nav:settings");
    expect(attr(renderNavItem(item({ id: "help", label: "Help" })), "data-ui-bridge-id")).toBe(
      "nav:help",
    );
  });

  it("keeps the id stable across re-renders and unrelated prop changes", () => {
    const a = attr(renderNavItem(item()), "data-ui-bridge-id");
    const b = attr(
      renderToStaticMarkup(
        <NavItem
          item={item()}
          isActive
          isParentActive
          collapsed={false}
          onClick={() => {}}
          onKeyDown={() => {}}
          tabIndex={-1}
          dataNavItem="projects"
        />,
      ),
      "data-ui-bridge-id",
    );
    expect(a).toBe("nav:projects");
    expect(b).toBe(a);
  });

  it("leaves the persist opt-in and the accessible name untouched", () => {
    const html = renderNavItem(item());
    expect(attr(html, "data-ui-bridge-persist")).toBe("true");
    expect(attr(html, "aria-label")).toBe("Projects");
    expect(attr(html, "data-nav-item")).toBe("projects");
  });
});

describe("FlyoutItem addressability", () => {
  it("stamps the same `nav:<id>` vocabulary as NavItem", () => {
    expect(
      attr(renderFlyoutItem(item({ id: "run-recap", label: "Runs" })), "data-ui-bridge-id"),
    ).toBe("nav:run-recap");
  });

  it("leaves the persist opt-in and the accessible name untouched", () => {
    const html = renderFlyoutItem(item({ id: "run-recap", label: "Runs" }));
    expect(attr(html, "data-ui-bridge-persist")).toBe("true");
    expect(attr(html, "aria-label")).toBe("Runs");
  });
});
