/**
 * The app-wide error banner's controls carry accessible names, and the banner
 * renders in flow (plan
 * `2026-10-22-runner-terminal-overlays-cover-zone-content-and-controls` F1/F2).
 *
 * The old dismiss was a bare `<X>` inside `<button>` with no aria-label, title
 * or text, so the UI Bridge registered it as `button-child-1` with
 * `label: null`. vitest runs in `environment: "node"`, so these render through
 * `react-dom/server` as the other component contract tests do.
 */
import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { ErrorBanner, ErrorResidual } from "./ErrorBanner";
import type { ErrorEventPayload } from "./errorBannerState";

const NO_LEADER: ErrorEventPayload = {
  title: "Coord has no leader",
  message: "No coord replica is running the leader-gated plane.",
  error_code: "COORD_LIVENESS_NO_LEADER",
  severity: "warning",
  recoverable: true,
};

describe("ErrorBanner", () => {
  const html = renderToStaticMarkup(<ErrorBanner error={NO_LEADER} onDismiss={() => {}} />);

  it("names the dismiss control after the fault", () => {
    expect(html).toContain('aria-label="Dismiss: Coord has no leader"');
    expect(html).toContain('title="Dismiss: Coord has no leader"');
  });

  it("renders in flow, never as a fixed overlay", () => {
    expect(html).not.toMatch(/\bfixed\b/);
    expect(html).not.toMatch(/\bz-\d+/);
  });
});

describe("ErrorResidual", () => {
  const html = renderToStaticMarkup(
    <ErrorResidual error={NO_LEADER} lastSeenAt={0} now={125_000} onReopen={() => {}} />,
  );

  it("names the code and when it was last seen", () => {
    expect(html).toContain("COORD_LIVENESS_NO_LEADER · last seen 2m ago");
  });

  it("is a named button that re-opens the banner", () => {
    expect(html).toContain(
      'aria-label="Show Coord has no leader (COORD_LIVENESS_NO_LEADER · last seen 2m ago)"',
    );
  });
});
