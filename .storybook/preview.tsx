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
    backgrounds: {
      default: "dark",
      values: [
        {
          name: "dark",
          value: "hsl(0 0% 4%)",
        },
        {
          name: "light",
          value: "#ffffff",
        },
      ],
    },
    layout: "centered",
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
