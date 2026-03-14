/**
 * EmptyState Component Stories
 *
 * Storybook documentation for the EmptyState component.
 * Displays a standardized empty state with icon, title, and optional description.
 */

import type { Meta, StoryObj } from "@storybook/react";
import { Terminal, Monitor, Clock, Search } from "lucide-react";
import { EmptyState } from "./EmptyState";

const meta: Meta<typeof EmptyState> = {
  title: "UI/Shared/EmptyState",
  component: EmptyState,
  tags: ["autodocs"],
  parameters: {
    layout: "centered",
    docs: {
      description: {
        component: `
Standardized empty state display for dashboard widgets. Provides a consistent
visual pattern when a widget has no data to show.

## Usage

\`\`\`tsx
import { EmptyState } from "./EmptyState";
import { Terminal } from "lucide-react";

<EmptyState
  icon={Terminal}
  title="No commands yet"
  description="Commands will appear here as they execute"
/>
\`\`\`
        `,
      },
    },
  },
  argTypes: {
    icon: {
      control: false,
      description: "Lucide icon component to display",
    },
    title: {
      control: "text",
      description: "Title text displayed below the icon",
    },
    description: {
      control: "text",
      description: "Optional description text below the title",
    },
    className: {
      control: "text",
      description: "Additional CSS classes for custom sizing/spacing",
    },
  },
};

export default meta;
type Story = StoryObj<typeof EmptyState>;

// ===========================================
// Basic Variants
// ===========================================

export const Default: Story = {
  args: {
    icon: Terminal,
    title: "No commands yet",
    description: "Commands will appear here as they execute",
  },
};

export const WithoutDescription: Story = {
  args: {
    icon: Monitor,
    title: "No active sessions",
  },
  parameters: {
    docs: {
      description: {
        story: "Empty state with just an icon and title, no description text",
      },
    },
  },
};

export const CustomClassName: Story = {
  args: {
    icon: Clock,
    title: "No recent activity",
    description: "Activity will be logged here",
    className: "py-8 min-h-[200px]",
  },
  parameters: {
    docs: {
      description: {
        story: "Empty state with custom className for adjusted sizing and spacing",
      },
    },
  },
};

// ===========================================
// Multiple Icons Showcase
// ===========================================

export const IconVariations: Story = {
  render: () => {
    const examples = [
      { icon: Terminal, title: "No commands", description: "Run a command to get started" },
      { icon: Monitor, title: "No sessions", description: "Start a session to begin" },
      { icon: Clock, title: "No history", description: "History will appear here" },
      { icon: Search, title: "No results", description: "Try adjusting your search" },
    ] as const;

    return (
      <div className="grid grid-cols-2 gap-4">
        {examples.map(({ icon, title, description }) => (
          <div key={title} className="border border-border rounded-lg">
            <EmptyState icon={icon} title={title} description={description} />
          </div>
        ))}
      </div>
    );
  },
  parameters: {
    docs: {
      description: {
        story: "Multiple empty states showing different icon and text combinations",
      },
    },
  },
};
