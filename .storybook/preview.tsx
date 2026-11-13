import type { Preview } from "@storybook/react";
import { UIBridgeProvider } from "@qontinui/ui-bridge";
import "../src/index.css";

const preview: Preview = {
  parameters: {
    controls: {
      matchers: {
        color: /(background|color)$/i,
        date: /Date$/i,
      },
    },
    layout: "centered",
  },
  initialGlobals: {
    backgrounds: { value: "dark" },
  },
  decorators: [
    // UIBridgeProvider makes every story visible to UI Bridge, so a story can
    // be driven through `ui-bridge-inject` and captured with
    // `GET /control/component/:id/capture` — the isolated side of the
    // `matches_isolated_render` vision assertion (plan
    // 2026-11-13-ui-bridge-matches-isolated-render-assertion). The centered
    // layout and padding wrapper stay as they were: the assertion compares the
    // root's fill ratio of its parent, so a fluid component shrink-wrapping
    // here is not read as a size divergence.
    (Story) => (
      <UIBridgeProvider features={{ control: true, debug: true }}>
        <div className="dark" style={{ padding: "2rem" }}>
          <Story />
        </div>
      </UIBridgeProvider>
    ),
  ],
};

export default preview;
