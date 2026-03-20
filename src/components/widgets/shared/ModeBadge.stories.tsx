/**
 * ModeBadge Component Stories
 *
 * Storybook documentation for the ModeBadge component.
 * Displays a filled badge for mode/category labels using accent colors.
 */

import type { Meta, StoryObj } from "storybook";
import { ModeBadge } from "./ModeBadge";
import type { AccentColor } from "@/design-system/colors";

const meta: Meta<typeof ModeBadge> = {
  title: "UI/Shared/ModeBadge",
  component: ModeBadge,
  tags: ["autodocs"],
  parameters: {
    layout: "centered",
    docs: {
      description: {
        component: `
Standardized filled badge for displaying mode/category labels. Uses the design
system's accent colors for consistent coloring across the application.

## Usage

\`\`\`tsx
import { ModeBadge } from "./ModeBadge";

<ModeBadge label="SH" color="slate" count={3} />
<ModeBadge label="CHK" color="blue" count={2} />
<ModeBadge label="shell" color="green" />
\`\`\`
        `,
      },
    },
  },
  argTypes: {
    label: {
      control: "text",
      description: "Badge label text",
    },
    color: {
      control: "select",
      options: [
        "red",
        "orange",
        "amber",
        "yellow",
        "green",
        "emerald",
        "blue",
        "cyan",
        "purple",
        "slate",
      ],
      description: "Accent color from the design system",
    },
    count: {
      control: { type: "number", min: 0 },
      description: "Optional count displayed after the label",
    },
    className: {
      control: "text",
      description: "Additional CSS classes",
    },
  },
};

export default meta;
type Story = StoryObj<typeof ModeBadge>;

// ===========================================
// Basic Variants
// ===========================================

export const Shell: Story = {
  args: {
    label: "SH",
    color: "slate",
    count: 3,
  },
};

export const Check: Story = {
  args: {
    label: "CHK",
    color: "blue",
    count: 2,
  },
};

export const Test: Story = {
  args: {
    label: "TST",
    color: "purple",
    count: 1,
  },
};

export const WithoutCount: Story = {
  args: {
    label: "shell",
    color: "green",
  },
  parameters: {
    docs: {
      description: {
        story: "Badge without a count, showing just the label",
      },
    },
  },
};

// ===========================================
// All Colors
// ===========================================

export const AllColors: Story = {
  render: () => {
    const colors: Array<{ color: AccentColor; label: string }> = [
      { color: "red", label: "RED" },
      { color: "orange", label: "ORG" },
      { color: "amber", label: "AMB" },
      { color: "yellow", label: "YLW" },
      { color: "green", label: "GRN" },
      { color: "emerald", label: "EMR" },
      { color: "blue", label: "BLU" },
      { color: "cyan", label: "CYN" },
      { color: "purple", label: "PUR" },
      { color: "slate", label: "SLT" },
    ];

    return (
      <div className="flex flex-wrap gap-2">
        {colors.map(({ color, label }) => (
          <ModeBadge key={color} label={label} color={color} count={2} />
        ))}
      </div>
    );
  },
  parameters: {
    docs: {
      description: {
        story: "All available accent colors displayed side by side",
      },
    },
  },
};
