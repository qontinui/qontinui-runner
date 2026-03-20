import type { Preview } from "@storybook/react";
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
    (Story) => (
      <div className="dark" style={{ padding: "2rem" }}>
        <Story />
      </div>
    ),
  ],
};

export default preview;
