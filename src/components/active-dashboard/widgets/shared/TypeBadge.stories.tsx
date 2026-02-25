/**
 * TypeBadge Component Stories
 *
 * Storybook documentation for the TypeBadge component.
 * Displays an outline badge for step/action types using accent colors.
 */

import type { Meta, StoryObj } from "@storybook/react";
import { CheckCircle, Play, Settings, Zap } from "lucide-react";
import { TypeBadge } from "./TypeBadge";
import type { AccentColor } from "../../../../design-system/colors";

const meta: Meta<typeof TypeBadge> = {
  title: "UI/Shared/TypeBadge",
  component: TypeBadge,
  tags: ["autodocs"],
  parameters: {
    layout: "centered",
    docs: {
      description: {
        component: `
Standardized outline badge for displaying step/action types. Uses the design
system's accent colors with a border-only (outline) style, distinct from
ModeBadge which uses a filled background.

## Usage

\`\`\`tsx
import { TypeBadge } from "./TypeBadge";
import { CheckCircle } from "lucide-react";

<TypeBadge label="command" color="blue" />
<TypeBadge label="verification" color="green" icon={<CheckCircle className="h-3 w-3" />} />
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
    icon: {
      control: false,
      description: "Optional React node icon displayed before the label",
    },
    className: {
      control: "text",
      description: "Additional CSS classes",
    },
  },
};

export default meta;
type Story = StoryObj<typeof TypeBadge>;

// ===========================================
// Basic Variants
// ===========================================

export const Default: Story = {
  args: {
    label: "command",
    color: "blue",
  },
};

export const WithIcon: Story = {
  args: {
    label: "verification",
    color: "green",
    icon: <CheckCircle className="h-3 w-3" />,
  },
  parameters: {
    docs: {
      description: {
        story: "Badge with a leading icon for additional visual context",
      },
    },
  },
};

// ===========================================
// All Colors
// ===========================================

export const AllColors: Story = {
  render: () => {
    const colors: Array<{ color: AccentColor; label: string; icon?: React.ReactNode }> = [
      { color: "red", label: "error" },
      { color: "orange", label: "warning" },
      { color: "amber", label: "caution" },
      { color: "yellow", label: "notice" },
      { color: "green", label: "verification", icon: <CheckCircle className="h-3 w-3" /> },
      { color: "emerald", label: "success" },
      { color: "blue", label: "command", icon: <Play className="h-3 w-3" /> },
      { color: "cyan", label: "analysis" },
      { color: "purple", label: "action", icon: <Zap className="h-3 w-3" /> },
      { color: "slate", label: "config", icon: <Settings className="h-3 w-3" /> },
    ];

    return (
      <div className="flex flex-wrap gap-2">
        {colors.map(({ color, label, icon }) => (
          <TypeBadge key={color} label={label} color={color} icon={icon} />
        ))}
      </div>
    );
  },
  parameters: {
    docs: {
      description: {
        story: "All available accent colors displayed side by side, some with icons",
      },
    },
  },
};
