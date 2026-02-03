/**
 * ProgressBar Component Stories
 *
 * Comprehensive Storybook documentation for the ProgressBar component.
 * Showcases all variants, states, sizes, and progress types.
 */

import type { Meta, StoryObj } from "@storybook/react";
import { useEffect, useState } from "react";
import { ProgressBar, type ProgressType } from "./ProgressBar";

const meta: Meta<typeof ProgressBar> = {
  title: "UI/Progress/ProgressBar",
  component: ProgressBar,
  tags: ["autodocs"],
  parameters: {
    layout: "centered",
    docs: {
      description: {
        component: `
A flexible progress bar component with support for:
- **Percentage or current/total display** - Show progress in different formats
- **Semantic colors** - Different colors based on progress type (files, tests, analysis, etc.)
- **Indeterminate mode** - Animated progress for unknown totals
- **Animated transitions** - Smooth width changes and pulsing effects
- **Success/error states** - Visual feedback for completed progress

## Usage

\`\`\`tsx
import { ProgressBar } from "@/components/ui/ProgressBar";

// Basic usage
<ProgressBar current={50} total={100} />

// With label
<ProgressBar current={5} total={10} showLabel labelFormat="count" />

// Indeterminate mode
<ProgressBar current={42} indeterminate />

// Different progress type
<ProgressBar current={3} total={10} progressType="test_progress" />
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
      description: "Total value (if known)",
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
    status: {
      control: "select",
      options: ["idle", "active", "success", "error"],
      description: "Override the auto-detected status",
    },
    indeterminate: {
      control: "boolean",
      description: "Show indeterminate animation when total is unknown",
    },
    showLabel: {
      control: "boolean",
      description: "Whether to show label",
    },
    labelFormat: {
      control: "select",
      options: ["percentage", "count", "both", "none"],
      description: "Label format",
    },
    label: {
      control: "text",
      description: "Custom label text (overrides automatic label)",
    },
    description: {
      control: "text",
      description: "Description text below progress bar",
    },
    size: {
      control: "select",
      options: ["xs", "sm", "md", "lg"],
      description: "Size variant",
    },
  },
  decorators: [
    (Story) => (
      <div style={{ width: "300px" }}>
        <Story />
      </div>
    ),
  ],
};

export default meta;
type Story = StoryObj<typeof ProgressBar>;

// ===========================================
// Basic Variants
// ===========================================

export const Default: Story = {
  args: {
    current: 50,
    total: 100,
  },
};

export const WithLabel: Story = {
  args: {
    current: 50,
    total: 100,
    showLabel: true,
    labelFormat: "count",
  },
};

export const Percentage: Story = {
  args: {
    current: 75,
    total: 100,
    showLabel: true,
    labelFormat: "percentage",
  },
};

export const CountLabel: Story = {
  args: {
    current: 50,
    total: 100,
    showLabel: true,
    labelFormat: "count",
  },
  parameters: {
    docs: {
      description: {
        story: 'Shows progress as "current/total" format (e.g., "50/100")',
      },
    },
  },
};

export const BothFormats: Story = {
  args: {
    current: 35,
    total: 100,
    showLabel: true,
    labelFormat: "both",
  },
  parameters: {
    docs: {
      description: {
        story: 'Shows both count and percentage (e.g., "35/100 (35%)")',
      },
    },
  },
};

export const WithDescription: Story = {
  args: {
    current: 25,
    total: 100,
    showLabel: true,
    labelFormat: "count",
    description: "Processing files...",
  },
};

export const CustomLabel: Story = {
  args: {
    current: 42,
    total: 100,
    label: "Almost halfway there!",
  },
};

// ===========================================
// Progress Types (Colors)
// ===========================================

export const FileProgress: Story = {
  args: {
    current: 15,
    total: 50,
    progressType: "file_progress",
    showLabel: true,
    labelFormat: "count",
    description: "Processing files",
  },
  parameters: {
    docs: {
      description: {
        story: "Blue color scheme for file-related progress",
      },
    },
  },
};

export const TestProgress: Story = {
  args: {
    current: 8,
    total: 20,
    progressType: "test_progress",
    showLabel: true,
    labelFormat: "count",
    description: "Running tests",
  },
  parameters: {
    docs: {
      description: {
        story: "Purple color scheme for test execution progress",
      },
    },
  },
};

export const AnalysisProgress: Story = {
  args: {
    current: 3,
    total: 10,
    progressType: "analysis_progress",
    showLabel: true,
    labelFormat: "count",
    description: "Analyzing code",
  },
  parameters: {
    docs: {
      description: {
        story: "Cyan color scheme for analysis tasks",
      },
    },
  },
};

export const ReviewProgress: Story = {
  args: {
    current: 12,
    total: 30,
    progressType: "review_progress",
    showLabel: true,
    labelFormat: "count",
    description: "Review items",
  },
  parameters: {
    docs: {
      description: {
        story: "Amber color scheme for review progress",
      },
    },
  },
};

export const IterationProgress: Story = {
  args: {
    current: 2,
    total: 5,
    progressType: "iteration_progress",
    showLabel: true,
    labelFormat: "both",
    description: "Iteration cycle",
  },
  parameters: {
    docs: {
      description: {
        story: "Emerald color scheme for iteration/loop progress",
      },
    },
  },
};

// ===========================================
// States
// ===========================================

export const Indeterminate: Story = {
  args: {
    current: 42,
    indeterminate: true,
    showLabel: true,
    labelFormat: "count",
    description: "Loading...",
  },
  parameters: {
    docs: {
      description: {
        story:
          "Animated indeterminate state for when the total is unknown. The bar shows a sliding animation.",
      },
    },
  },
};

export const IndeterminateWithNullTotal: Story = {
  args: {
    current: 15,
    total: null,
    showLabel: true,
    labelFormat: "count",
    description: "Total unknown",
  },
  parameters: {
    docs: {
      description: {
        story: "Automatically enters indeterminate mode when total is null",
      },
    },
  },
};

export const Active: Story = {
  args: {
    current: 60,
    total: 100,
    status: "active",
    showLabel: true,
    labelFormat: "percentage",
    description: "In progress",
  },
  parameters: {
    docs: {
      description: {
        story: "Active state shows a subtle pulse animation on the progress fill",
      },
    },
  },
};

export const Success: Story = {
  args: {
    current: 100,
    total: 100,
    status: "success",
    showLabel: true,
    labelFormat: "count",
    description: "Completed!",
  },
  parameters: {
    docs: {
      description: {
        story: "Success state with green color, auto-detected when current >= total",
      },
    },
  },
};

export const Error: Story = {
  args: {
    current: 45,
    total: 100,
    status: "error",
    showLabel: true,
    labelFormat: "percentage",
    description: "Failed at 45%",
  },
  parameters: {
    docs: {
      description: {
        story: "Error state with red color for failed operations",
      },
    },
  },
};

export const Idle: Story = {
  args: {
    current: 0,
    total: 100,
    status: "idle",
    showLabel: true,
    labelFormat: "percentage",
    description: "Not started",
  },
  parameters: {
    docs: {
      description: {
        story: "Idle state before progress begins (current = 0)",
      },
    },
  },
};

// ===========================================
// Sizes
// ===========================================

export const ExtraSmall: Story = {
  args: {
    current: 60,
    total: 100,
    size: "xs",
    showLabel: true,
    labelFormat: "percentage",
  },
  parameters: {
    docs: {
      description: {
        story: "Extra small size (h-1) for compact UI elements",
      },
    },
  },
};

export const Small: Story = {
  args: {
    current: 60,
    total: 100,
    size: "sm",
    showLabel: true,
    labelFormat: "percentage",
  },
  parameters: {
    docs: {
      description: {
        story: "Small size (h-1.5) for inline progress indicators",
      },
    },
  },
};

export const Medium: Story = {
  args: {
    current: 60,
    total: 100,
    size: "md",
    showLabel: true,
    labelFormat: "percentage",
  },
  parameters: {
    docs: {
      description: {
        story: "Medium size (h-2) - default size",
      },
    },
  },
};

export const Large: Story = {
  args: {
    current: 60,
    total: 100,
    size: "lg",
    showLabel: true,
    labelFormat: "percentage",
  },
  parameters: {
    docs: {
      description: {
        story: "Large size (h-3) for prominent progress displays",
      },
    },
  },
};

// ===========================================
// Interactive / Animated
// ===========================================

/**
 * Animated story that demonstrates progress incrementing.
 */
function AnimatedProgressComponent() {
  const [progress, setProgress] = useState(0);

  useEffect(() => {
    const interval = setInterval(() => {
      setProgress((prev) => {
        if (prev >= 100) return 0;
        return prev + 5;
      });
    }, 200);
    return () => clearInterval(interval);
  }, []);

  return (
    <ProgressBar
      current={progress}
      total={100}
      showLabel
      labelFormat="percentage"
      progressType="file_progress"
      description="Uploading files..."
    />
  );
}

export const Animated: Story = {
  render: () => <AnimatedProgressComponent />,
  parameters: {
    docs: {
      description: {
        story: "Demonstrates animated progress with smooth transitions as the value increments.",
      },
    },
  },
};

/**
 * Story showing all progress types side by side.
 */
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
      <div className="space-y-4 w-full">
        {progressTypes.map((type) => (
          <div key={type}>
            <span className="text-xs text-muted-foreground mb-1 block">{type}</span>
            <ProgressBar
              current={65}
              total={100}
              progressType={type}
              showLabel
              labelFormat="percentage"
            />
          </div>
        ))}
      </div>
    );
  },
  decorators: [
    (Story) => (
      <div style={{ width: "350px" }}>
        <Story />
      </div>
    ),
  ],
  parameters: {
    docs: {
      description: {
        story: "Comparison of all available progress type color schemes",
      },
    },
  },
};

/**
 * Story showing all sizes side by side.
 */
export const AllSizes: Story = {
  render: () => {
    const sizes = ["xs", "sm", "md", "lg"] as const;

    return (
      <div className="space-y-4 w-full">
        {sizes.map((size) => (
          <div key={size}>
            <span className="text-xs text-muted-foreground mb-1 block">{size.toUpperCase()}</span>
            <ProgressBar current={60} total={100} size={size} showLabel labelFormat="percentage" />
          </div>
        ))}
      </div>
    );
  },
  decorators: [
    (Story) => (
      <div style={{ width: "350px" }}>
        <Story />
      </div>
    ),
  ],
  parameters: {
    docs: {
      description: {
        story: "Comparison of all available size variants",
      },
    },
  },
};

/**
 * Story showing all states side by side.
 */
export const AllStates: Story = {
  render: () => {
    return (
      <div className="space-y-4 w-full">
        <div>
          <span className="text-xs text-muted-foreground mb-1 block">Idle (0%)</span>
          <ProgressBar current={0} total={100} showLabel labelFormat="percentage" />
        </div>
        <div>
          <span className="text-xs text-muted-foreground mb-1 block">Active (pulse)</span>
          <ProgressBar
            current={45}
            total={100}
            status="active"
            showLabel
            labelFormat="percentage"
          />
        </div>
        <div>
          <span className="text-xs text-muted-foreground mb-1 block">Indeterminate</span>
          <ProgressBar current={23} indeterminate showLabel labelFormat="count" />
        </div>
        <div>
          <span className="text-xs text-muted-foreground mb-1 block">Success</span>
          <ProgressBar
            current={100}
            total={100}
            status="success"
            showLabel
            labelFormat="percentage"
          />
        </div>
        <div>
          <span className="text-xs text-muted-foreground mb-1 block">Error</span>
          <ProgressBar current={67} total={100} status="error" showLabel labelFormat="percentage" />
        </div>
      </div>
    );
  },
  decorators: [
    (Story) => (
      <div style={{ width: "350px" }}>
        <Story />
      </div>
    ),
  ],
  parameters: {
    docs: {
      description: {
        story: "Comparison of all available states",
      },
    },
  },
};
