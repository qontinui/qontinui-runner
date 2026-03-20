/**
 * StepProgressMarker Component Stories
 *
 * Storybook documentation for StepProgressMarker and StepProgressIndicator.
 * These components display intra-step progress for long-running operations.
 */

import type { Meta, StoryObj } from "storybook";
import { StepProgressIndicator } from "./StepProgressMarker";

// Note: StepProgressMarker fetches data from an API, so we'll focus on
// the StepProgressIndicator which is purely presentational, and mock
// the StepProgressMarker scenarios.

const meta: Meta<typeof StepProgressIndicator> = {
  title: "UI/Progress/StepProgressIndicator",
  component: StepProgressIndicator,
  tags: ["autodocs"],
  parameters: {
    layout: "centered",
    docs: {
      description: {
        component: `
A simplified inline progress indicator for step rows. This is a wrapper around InlineProgressBar
with semantic mapping from marker types to progress colors.

**StepProgressMarker** - Full component that:
- Fetches progress data from the API
- Auto-refreshes for running steps
- Shows loading states
- Displays both compact and full modes

**StepProgressIndicator** - Simplified component that:
- Takes progress values directly
- Maps marker types to colors
- Purely presentational

## Marker Types and Colors

| Marker Type | Color | Use Case |
|-------------|-------|----------|
| file_progress | Blue | File operations |
| test_progress | Purple | Test execution |
| analysis_progress | Cyan | Code analysis |
| review_progress | Amber | Review items |
| iteration_progress | Emerald | Iteration cycles |

## Usage

\`\`\`tsx
import { StepProgressIndicator, StepProgressMarker } from "./StepProgressMarker";

// Simple indicator (presentational)
<StepProgressIndicator current={5} total={10} type="file_progress" />

// Full marker with API fetching (for running steps)
<StepProgressMarker
  taskRunId="run-123"
  checkpointId="step-1"
  autoRefresh
  compact
/>
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
    type: {
      control: "select",
      options: [
        "file_progress",
        "test_progress",
        "analysis_progress",
        "review_progress",
        "iteration_progress",
      ],
      description: "Marker type determines color",
    },
    className: {
      control: "text",
      description: "Additional CSS classes",
    },
  },
};

export default meta;
type Story = StoryObj<typeof StepProgressIndicator>;

// ===========================================
// Basic Variants
// ===========================================

export const Default: Story = {
  args: {
    current: 5,
    total: 10,
  },
};

export const FileProgress: Story = {
  args: {
    current: 25,
    total: 100,
    type: "file_progress",
  },
  parameters: {
    docs: {
      description: {
        story: "Blue progress bar for file operations",
      },
    },
  },
};

export const TestProgress: Story = {
  args: {
    current: 8,
    total: 20,
    type: "test_progress",
  },
  parameters: {
    docs: {
      description: {
        story: "Purple progress bar for test execution",
      },
    },
  },
};

export const AnalysisProgress: Story = {
  args: {
    current: 3,
    total: null,
    type: "analysis_progress",
  },
  parameters: {
    docs: {
      description: {
        story: "Cyan indeterminate progress for analysis (unknown total)",
      },
    },
  },
};

export const ReviewProgress: Story = {
  args: {
    current: 12,
    total: 15,
    type: "review_progress",
  },
  parameters: {
    docs: {
      description: {
        story: "Amber progress bar for review items",
      },
    },
  },
};

export const IterationProgress: Story = {
  args: {
    current: 2,
    total: 5,
    type: "iteration_progress",
  },
  parameters: {
    docs: {
      description: {
        story: "Emerald progress bar for iteration cycles",
      },
    },
  },
};

export const Complete: Story = {
  args: {
    current: 10,
    total: 10,
    type: "test_progress",
  },
  parameters: {
    docs: {
      description: {
        story: "Completed progress (success state - green)",
      },
    },
  },
};

export const Indeterminate: Story = {
  args: {
    current: 42,
    total: null,
    type: "file_progress",
  },
  parameters: {
    docs: {
      description: {
        story: "Indeterminate progress when total is unknown",
      },
    },
  },
};

// ===========================================
// All Types Comparison
// ===========================================

export const AllMarkerTypes: Story = {
  render: () => {
    const types = [
      { type: "file_progress", label: "Files" },
      { type: "test_progress", label: "Tests" },
      { type: "analysis_progress", label: "Analysis" },
      { type: "review_progress", label: "Review" },
      { type: "iteration_progress", label: "Iteration" },
    ] as const;

    return (
      <div className="space-y-3">
        {types.map(({ type, label }) => (
          <div key={type} className="flex items-center gap-4">
            <span className="text-xs text-muted-foreground w-24">{label}</span>
            <StepProgressIndicator current={6} total={10} type={type} />
          </div>
        ))}
      </div>
    );
  },
  parameters: {
    docs: {
      description: {
        story: "All marker types with their semantic colors",
      },
    },
  },
};

// ===========================================
// Step Row Context
// ===========================================

export const InStepRowContext: Story = {
  render: () => {
    const steps = [
      {
        id: 1,
        name: "Setup Environment",
        status: "complete",
        current: 5,
        total: 5,
        type: "file_progress",
      },
      { id: 2, name: "Run Tests", status: "running", current: 8, total: 20, type: "test_progress" },
      {
        id: 3,
        name: "Analyze Results",
        status: "pending",
        current: 0,
        total: 10,
        type: "analysis_progress",
      },
      {
        id: 4,
        name: "Generate Report",
        status: "pending",
        current: 0,
        total: null,
        type: "review_progress",
      },
    ] as const;

    const getStatusBadge = (status: string) => {
      const styles: Record<string, string> = {
        complete: "bg-green-500/10 text-green-500",
        running: "bg-blue-500/10 text-blue-400",
        pending: "bg-muted/50 text-muted-foreground",
      };
      return styles[status] || styles.pending;
    };

    return (
      <div className="space-y-1">
        {steps.map((step) => (
          <div
            key={step.id}
            className="flex items-center gap-3 py-2 px-3 rounded hover:bg-muted/30"
          >
            <span className="text-sm font-medium flex-1">{step.name}</span>
            <span className={`text-xs px-2 py-0.5 rounded ${getStatusBadge(step.status)}`}>
              {step.status}
            </span>
            {(step.status === "running" || step.status === "complete") && (
              <StepProgressIndicator current={step.current} total={step.total} type={step.type} />
            )}
          </div>
        ))}
      </div>
    );
  },
  decorators: [
    (Story) => (
      <div style={{ width: "450px" }} className="bg-card/50 rounded-lg p-2">
        <Story />
      </div>
    ),
  ],
  parameters: {
    docs: {
      description: {
        story: "Example showing StepProgressIndicator integrated into a step list UI",
      },
    },
  },
};

// ===========================================
// Mock Full StepProgressMarker Display
// ===========================================

/**
 * This simulates what StepProgressMarker would display with real data.
 * The actual component fetches from API, but this shows the visual output.
 */
export const MockFullDisplay: Story = {
  render: () => {
    // Simulated progress marker data
    const mockMarkers: Array<{
      type: string;
      current: number;
      total: number | null;
      description: string | null;
    }> = [
      { type: "file_progress", current: 45, total: 100, description: "Processing source files" },
      { type: "test_progress", current: 8, total: 20, description: "Running unit tests" },
      {
        type: "analysis_progress",
        current: 12,
        total: null,
        description: "Analyzing dependencies",
      },
    ];

    const getLabel = (type: string) => {
      const labels: Record<string, string> = {
        file_progress: "Files",
        test_progress: "Tests",
        analysis_progress: "Analysis",
      };
      return labels[type] || type;
    };

    return (
      <div className="space-y-4">
        {mockMarkers.map((marker) => (
          <div key={marker.type} className="space-y-1">
            <span className="text-xs font-medium">{getLabel(marker.type)}</span>
            <div className="flex items-center gap-2">
              <div className="flex-1">
                <StepProgressIndicator
                  current={marker.current}
                  total={marker.total}
                  type={marker.type}
                />
              </div>
              {marker.description && (
                <span className="text-xs text-muted-foreground">{marker.description}</span>
              )}
            </div>
          </div>
        ))}
      </div>
    );
  },
  decorators: [
    (Story) => (
      <div style={{ width: "400px" }}>
        <Story />
      </div>
    ),
  ],
  parameters: {
    docs: {
      description: {
        story: `
Simulates the full StepProgressMarker display with:
- Type labels
- Progress bars with counts
- Optional descriptions

Note: The actual StepProgressMarker component fetches data from the API endpoint
\`/task-runs/{taskRunId}/steps/{checkpointId}/progress\`
        `,
      },
    },
  },
};

// ===========================================
// Compact vs Full Comparison
// ===========================================

export const CompactVsFull: Story = {
  render: () => {
    const data = { current: 25, total: 100, type: "test_progress" as const };

    return (
      <div className="space-y-6">
        <div>
          <h4 className="text-xs font-medium text-muted-foreground mb-2">Compact Mode</h4>
          <div className="flex items-center gap-2 bg-muted/30 p-2 rounded">
            <StepProgressIndicator {...data} />
            <span className="text-xs text-muted-foreground">Tests</span>
          </div>
        </div>

        <div>
          <h4 className="text-xs font-medium text-muted-foreground mb-2">Full Mode</h4>
          <div className="bg-muted/30 p-3 rounded space-y-1">
            <span className="text-xs font-medium">Tests</span>
            <div className="flex items-center gap-2">
              <div className="flex-1">
                <StepProgressIndicator {...data} />
              </div>
              <span className="text-xs text-muted-foreground">Running test suite</span>
            </div>
          </div>
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
        story: `
Comparison of compact and full display modes:
- **Compact**: Single line with inline progress and type label
- **Full**: Header label, progress bar, and description
        `,
      },
    },
  },
};
