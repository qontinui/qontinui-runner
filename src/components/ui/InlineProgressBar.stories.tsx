/**
 * InlineProgressBar Component Stories
 *
 * Storybook documentation for the compact InlineProgressBar component.
 * Used for progress display in tables, lists, and other compact UI contexts.
 */

import type { Meta, StoryObj } from "@storybook/react";
import { InlineProgressBar, type ProgressType } from "./ProgressBar";

const meta: Meta<typeof InlineProgressBar> = {
  title: "UI/Progress/InlineProgressBar",
  component: InlineProgressBar,
  tags: ["autodocs"],
  parameters: {
    layout: "centered",
    docs: {
      description: {
        component: `
A compact inline progress indicator designed for use in tables, lists, and other space-constrained contexts.

Features:
- **Fixed width** - 48px progress bar for consistent alignment
- **Inline layout** - Horizontal bar + count label
- **Same color system** - Uses the same semantic colors as ProgressBar
- **Indeterminate support** - Animated when total is null

## Usage

\`\`\`tsx
import { InlineProgressBar } from "@/components/ui/ProgressBar";

// In a table cell
<InlineProgressBar current={5} total={10} progressType="file_progress" />

// Unknown total (indeterminate)
<InlineProgressBar current={15} total={null} progressType="analysis_progress" />
\`\`\`
        `,
      },
    },
  },
  argTypes: {
    current: {
      control: { type: "number", min: 0, max: 100 },
      description: "Current progress value",
    },
    total: {
      control: { type: "number", min: 0, max: 100 },
      description: "Total value (null for indeterminate)",
    },
    progressType: {
      control: "select",
      options: [
        "default",
        "file_progress",
        "test_progress",
        "analysis_progress",
        "review_progress",
        "iteration_progress",
      ],
      description: "Progress type determines color scheme",
    },
    className: {
      control: "text",
      description: "Additional CSS classes",
    },
  },
};

export default meta;
type Story = StoryObj<typeof InlineProgressBar>;

// ===========================================
// Basic Variants
// ===========================================

export const Default: Story = {
  args: {
    current: 5,
    total: 10,
  },
};

export const WithProgress: Story = {
  args: {
    current: 75,
    total: 100,
  },
};

export const Complete: Story = {
  args: {
    current: 10,
    total: 10,
  },
  parameters: {
    docs: {
      description: {
        story: "Shows success state (green) when current equals total",
      },
    },
  },
};

export const Empty: Story = {
  args: {
    current: 0,
    total: 10,
  },
};

export const Indeterminate: Story = {
  args: {
    current: 15,
    total: null,
  },
  parameters: {
    docs: {
      description: {
        story: "Animated indeterminate state when total is null. Only shows current value.",
      },
    },
  },
};

// ===========================================
// Progress Types (Colors)
// ===========================================

export const FileProgress: Story = {
  args: {
    current: 12,
    total: 50,
    progressType: "file_progress",
  },
  parameters: {
    docs: {
      description: {
        story: "Blue color for file operations",
      },
    },
  },
};

export const TestProgress: Story = {
  args: {
    current: 8,
    total: 20,
    progressType: "test_progress",
  },
  parameters: {
    docs: {
      description: {
        story: "Purple color for test execution",
      },
    },
  },
};

export const AnalysisProgress: Story = {
  args: {
    current: 3,
    total: 10,
    progressType: "analysis_progress",
  },
  parameters: {
    docs: {
      description: {
        story: "Cyan color for analysis tasks",
      },
    },
  },
};

export const ReviewProgress: Story = {
  args: {
    current: 5,
    total: 15,
    progressType: "review_progress",
  },
  parameters: {
    docs: {
      description: {
        story: "Amber color for review items",
      },
    },
  },
};

export const IterationProgress: Story = {
  args: {
    current: 2,
    total: 5,
    progressType: "iteration_progress",
  },
  parameters: {
    docs: {
      description: {
        story: "Emerald color for iteration cycles",
      },
    },
  },
};

// ===========================================
// All Types Comparison
// ===========================================

export const AllProgressTypes: Story = {
  render: () => {
    const progressTypes: ProgressType[] = [
      "default",
      "file_progress",
      "test_progress",
      "analysis_progress",
      "review_progress",
      "iteration_progress",
    ];

    return (
      <div className="space-y-3">
        {progressTypes.map((type) => (
          <div key={type} className="flex items-center gap-4">
            <span className="text-xs text-muted-foreground w-32">{type}</span>
            <InlineProgressBar current={6} total={10} progressType={type} />
          </div>
        ))}
      </div>
    );
  },
  parameters: {
    docs: {
      description: {
        story: "All progress type colors shown side by side",
      },
    },
  },
};

// ===========================================
// Table Context Example
// ===========================================

export const InTableContext: Story = {
  render: () => {
    const rows = [
      { name: "Process Files", current: 45, total: 100, type: "file_progress" as const },
      { name: "Run Tests", current: 8, total: 20, type: "test_progress" as const },
      { name: "Analyze Code", current: 3, total: null, type: "analysis_progress" as const },
      { name: "Review Changes", current: 15, total: 15, type: "review_progress" as const },
    ];

    return (
      <table className="w-full text-sm">
        <thead>
          <tr className="border-b border-border/50">
            <th className="text-left py-2 px-3 text-muted-foreground font-medium">Task</th>
            <th className="text-left py-2 px-3 text-muted-foreground font-medium">Progress</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => (
            <tr key={row.name} className="border-b border-border/30">
              <td className="py-2 px-3">{row.name}</td>
              <td className="py-2 px-3">
                <InlineProgressBar
                  current={row.current}
                  total={row.total}
                  progressType={row.type}
                />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    );
  },
  decorators: [
    (Story) => (
      <div style={{ width: "400px" }} className="bg-card/50 rounded-lg">
        <Story />
      </div>
    ),
  ],
  parameters: {
    docs: {
      description: {
        story:
          "Example showing InlineProgressBar used within a table for multiple task progress tracking",
      },
    },
  },
};

// ===========================================
// List Context Example
// ===========================================

export const InListContext: Story = {
  render: () => {
    const items = [
      { id: 1, name: "config.json", current: 1, total: 1 },
      { id: 2, name: "index.ts", current: 50, total: 100 },
      { id: 3, name: "utils.ts", current: 0, total: 45 },
      { id: 4, name: "main.py", current: 12, total: null },
    ];

    return (
      <div className="space-y-1">
        {items.map((item) => (
          <div
            key={item.id}
            className="flex items-center justify-between py-1.5 px-2 hover:bg-muted/30 rounded"
          >
            <span className="text-sm truncate">{item.name}</span>
            <InlineProgressBar
              current={item.current}
              total={item.total}
              progressType="file_progress"
            />
          </div>
        ))}
      </div>
    );
  },
  decorators: [
    (Story) => (
      <div style={{ width: "280px" }}>
        <Story />
      </div>
    ),
  ],
  parameters: {
    docs: {
      description: {
        story: "Example showing InlineProgressBar in a file list with various states",
      },
    },
  },
};
