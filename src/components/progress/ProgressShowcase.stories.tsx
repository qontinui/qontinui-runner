/**
 * Progress Components Showcase
 *
 * Combined stories showing all progress component variants together
 * for visual comparison and real-world usage examples.
 */

import type { Meta, StoryObj } from "@storybook/react";
import { useEffect, useState } from "react";
import { ProgressBar, InlineProgressBar, type ProgressType } from "../ui/ProgressBar";
import { StepProgressIndicator } from "../widgets/shared/StepProgressMarker";

// Wrapper component for stories
const ProgressShowcase = () => <div />;

const meta: Meta<typeof ProgressShowcase> = {
  title: "UI/Progress/Showcase",
  component: ProgressShowcase,
  tags: ["autodocs"],
  parameters: {
    layout: "padded",
    docs: {
      description: {
        component: `
# Progress Components Overview

This showcase presents all progress component variants together for visual comparison
and demonstrates real-world usage patterns.

## Components

| Component | Description | Use Case |
|-----------|-------------|----------|
| **ProgressBar** | Full-featured progress bar | Prominent progress displays |
| **InlineProgressBar** | Compact inline variant | Tables, lists, compact UI |
| **StepProgressIndicator** | Simplified step indicator | Step/workflow progress |

## Design System

All progress components share a consistent design language:

### Colors by Type
- **Default**: Blue
- **file_progress**: Blue
- **test_progress**: Purple
- **analysis_progress**: Cyan
- **review_progress**: Amber
- **iteration_progress**: Emerald

### Status Colors
- **Active**: Type color with pulse
- **Success**: Green
- **Error**: Red
- **Idle**: Muted
        `,
      },
    },
  },
};

export default meta;
type Story = StoryObj<typeof ProgressShowcase>;

// ===========================================
// All Variants Overview
// ===========================================

export const AllVariantsOverview: Story = {
  render: () => {
    return (
      <div className="space-y-8">
        {/* ProgressBar Section */}
        <section>
          <h2 className="text-lg font-semibold mb-4">ProgressBar</h2>
          <div className="grid grid-cols-2 gap-6">
            <div className="space-y-4">
              <h3 className="text-sm font-medium text-muted-foreground">Sizes</h3>
              {(["xs", "sm", "md", "lg"] as const).map((size) => (
                <div key={size} className="flex items-center gap-3">
                  <span className="text-xs text-muted-foreground w-8">{size}</span>
                  <div className="flex-1">
                    <ProgressBar current={60} total={100} size={size} />
                  </div>
                </div>
              ))}
            </div>
            <div className="space-y-4">
              <h3 className="text-sm font-medium text-muted-foreground">States</h3>
              {(["idle", "active", "success", "error"] as const).map((status) => (
                <div key={status} className="flex items-center gap-3">
                  <span className="text-xs text-muted-foreground w-12">{status}</span>
                  <div className="flex-1">
                    <ProgressBar
                      current={status === "idle" ? 0 : status === "success" ? 100 : 60}
                      total={100}
                      status={status}
                    />
                  </div>
                </div>
              ))}
            </div>
          </div>
        </section>

        {/* Progress Types Section */}
        <section>
          <h2 className="text-lg font-semibold mb-4">Progress Types (Colors)</h2>
          <div className="grid grid-cols-3 gap-4">
            {(
              [
                "default",
                "file_progress",
                "test_progress",
                "analysis_progress",
                "review_progress",
                "iteration_progress",
              ] as const
            ).map((type) => (
              <div key={type} className="p-3 bg-muted/30 rounded-lg">
                <span className="text-xs text-muted-foreground block mb-2">{type}</span>
                <ProgressBar current={65} total={100} progressType={type} />
              </div>
            ))}
          </div>
        </section>

        {/* InlineProgressBar Section */}
        <section>
          <h2 className="text-lg font-semibold mb-4">InlineProgressBar</h2>
          <div className="flex flex-wrap gap-6">
            {(
              ["default", "file_progress", "test_progress", "analysis_progress"] as ProgressType[]
            ).map((type) => (
              <div key={type} className="flex items-center gap-2">
                <InlineProgressBar current={7} total={12} progressType={type} />
                <span className="text-xs text-muted-foreground">
                  {type.replace("_progress", "")}
                </span>
              </div>
            ))}
          </div>
        </section>

        {/* StepProgressIndicator Section */}
        <section>
          <h2 className="text-lg font-semibold mb-4">StepProgressIndicator</h2>
          <div className="flex flex-wrap gap-6">
            {(
              [
                "file_progress",
                "test_progress",
                "analysis_progress",
                "review_progress",
                "iteration_progress",
              ] as const
            ).map((type) => (
              <div key={type} className="flex items-center gap-2">
                <StepProgressIndicator current={5} total={10} type={type} />
              </div>
            ))}
          </div>
        </section>
      </div>
    );
  },
  parameters: {
    docs: {
      description: {
        story: "Complete overview of all progress component variants, sizes, and states",
      },
    },
  },
};

// ===========================================
// Side-by-Side Comparisons
// ===========================================

export const ComponentComparison: Story = {
  render: () => {
    const progress = { current: 7, total: 12 };

    return (
      <div className="space-y-6">
        <h2 className="text-lg font-semibold">Same Data, Different Components</h2>
        <p className="text-sm text-muted-foreground">
          All showing {progress.current}/{progress.total} progress
        </p>

        <div className="space-y-4">
          <div className="flex items-center gap-4">
            <span className="text-xs text-muted-foreground w-32">ProgressBar (md)</span>
            <div className="w-64">
              <ProgressBar {...progress} showLabel labelFormat="count" />
            </div>
          </div>

          <div className="flex items-center gap-4">
            <span className="text-xs text-muted-foreground w-32">ProgressBar (sm)</span>
            <div className="w-64">
              <ProgressBar {...progress} size="sm" showLabel labelFormat="count" />
            </div>
          </div>

          <div className="flex items-center gap-4">
            <span className="text-xs text-muted-foreground w-32">InlineProgressBar</span>
            <InlineProgressBar {...progress} />
          </div>

          <div className="flex items-center gap-4">
            <span className="text-xs text-muted-foreground w-32">StepProgressIndicator</span>
            <StepProgressIndicator {...progress} type="default" />
          </div>
        </div>
      </div>
    );
  },
  parameters: {
    docs: {
      description: {
        story: "Direct comparison of different progress components with the same data",
      },
    },
  },
};

// ===========================================
// Real-World Usage: Step Card
// ===========================================

export const StepCardExample: Story = {
  render: () => {
    interface Step {
      id: number;
      name: string;
      status: "pending" | "running" | "complete" | "error";
      progress?: { current: number; total: number | null; type: ProgressType };
      duration?: string;
    }

    const steps: Step[] = [
      { id: 1, name: "Initialize Environment", status: "complete", duration: "0.5s" },
      { id: 2, name: "Load Configuration", status: "complete", duration: "0.2s" },
      {
        id: 3,
        name: "Process Files",
        status: "running",
        progress: { current: 45, total: 100, type: "file_progress" },
      },
      {
        id: 4,
        name: "Run Tests",
        status: "pending",
        progress: { current: 0, total: 20, type: "test_progress" },
      },
      {
        id: 5,
        name: "Generate Report",
        status: "pending",
      },
    ];

    const statusColors: Record<string, string> = {
      pending: "text-muted-foreground",
      running: "text-blue-400",
      complete: "text-green-500",
      error: "text-red-500",
    };

    const _statusIcons: Record<string, string> = {
      pending: "circle",
      running: "loader",
      complete: "check-circle",
      error: "x-circle",
    };

    return (
      <div className="bg-card/50 rounded-lg p-4 w-[400px]">
        <h3 className="text-sm font-semibold mb-4">Workflow Steps</h3>
        <div className="space-y-2">
          {steps.map((step) => (
            <div
              key={step.id}
              className={`p-3 rounded-lg ${step.status === "running" ? "bg-blue-500/10" : "bg-muted/20"}`}
            >
              <div className="flex items-center justify-between mb-1">
                <span className="text-sm font-medium">{step.name}</span>
                <span className={`text-xs ${statusColors[step.status]}`}>
                  {step.status === "complete" && step.duration}
                  {step.status === "running" && "In Progress"}
                  {step.status === "pending" && "Pending"}
                  {step.status === "error" && "Failed"}
                </span>
              </div>
              {step.progress && step.status !== "pending" && (
                <ProgressBar
                  current={step.progress.current}
                  total={step.progress.total}
                  progressType={step.progress.type}
                  size="sm"
                  showLabel
                  labelFormat="both"
                />
              )}
            </div>
          ))}
        </div>
      </div>
    );
  },
  parameters: {
    docs: {
      description: {
        story: "Real-world example: Workflow step cards with integrated progress bars",
      },
    },
  },
};

// ===========================================
// Real-World Usage: Dashboard Widget
// ===========================================

export const DashboardWidgetExample: Story = {
  render: () => {
    interface Task {
      name: string;
      current: number;
      total: number;
      type: ProgressType;
    }

    const tasks: Task[] = [
      { name: "File Processing", current: 156, total: 200, type: "file_progress" },
      { name: "Test Execution", current: 45, total: 120, type: "test_progress" },
      { name: "Code Analysis", current: 89, total: 100, type: "analysis_progress" },
      { name: "Review Items", current: 12, total: 30, type: "review_progress" },
    ];

    const overallProgress = tasks.reduce((acc, t) => acc + t.current, 0);
    const overallTotal = tasks.reduce((acc, t) => acc + t.total, 0);

    return (
      <div className="bg-card rounded-lg p-4 w-[350px]">
        <div className="flex items-center justify-between mb-4">
          <h3 className="text-sm font-semibold">Overall Progress</h3>
          <span className="text-2xl font-bold text-primary">
            {Math.round((overallProgress / overallTotal) * 100)}%
          </span>
        </div>

        <ProgressBar current={overallProgress} total={overallTotal} size="lg" className="mb-6" />

        <div className="space-y-3">
          {tasks.map((task) => (
            <div key={task.name} className="flex items-center justify-between">
              <span className="text-xs text-muted-foreground">{task.name}</span>
              <InlineProgressBar
                current={task.current}
                total={task.total}
                progressType={task.type}
              />
            </div>
          ))}
        </div>
      </div>
    );
  },
  parameters: {
    docs: {
      description: {
        story: "Real-world example: Dashboard widget showing overall and task-level progress",
      },
    },
  },
};

// ===========================================
// Real-World Usage: File Upload
// ===========================================

function FileUploadExampleComponent() {
  const [files, setFiles] = useState([
    { name: "document.pdf", size: "2.4 MB", progress: 100, status: "complete" as const },
    { name: "image.png", size: "1.2 MB", progress: 75, status: "uploading" as const },
    { name: "data.json", size: "456 KB", progress: 30, status: "uploading" as const },
    { name: "report.xlsx", size: "3.1 MB", progress: 0, status: "queued" as const },
  ]);

  // Simulate upload progress
  useEffect(() => {
    const interval = setInterval(() => {
      setFiles((prev) =>
        prev.map((file) => {
          if (file.status === "uploading" && file.progress < 100) {
            const newProgress = Math.min(100, file.progress + Math.random() * 10);
            return {
              ...file,
              progress: Math.round(newProgress),
              status: newProgress >= 100 ? "complete" : "uploading",
            };
          }
          if (
            file.status === "queued" &&
            prev.every((f) => f.status !== "uploading" || f.progress === 100)
          ) {
            return { ...file, status: "uploading" };
          }
          return file;
        }),
      );
    }, 500);

    return () => clearInterval(interval);
  }, []);

  return (
    <div className="bg-card/50 rounded-lg p-4 w-[400px]">
      <h3 className="text-sm font-semibold mb-4">Uploading Files</h3>
      <div className="space-y-3">
        {files.map((file) => (
          <div key={file.name} className="p-3 bg-muted/20 rounded-lg">
            <div className="flex items-center justify-between mb-2">
              <div>
                <span className="text-sm font-medium block">{file.name}</span>
                <span className="text-xs text-muted-foreground">{file.size}</span>
              </div>
              <span
                className={`text-xs ${
                  file.status === "complete"
                    ? "text-green-500"
                    : file.status === "uploading"
                      ? "text-blue-400"
                      : "text-muted-foreground"
                }`}
              >
                {file.status === "complete" && "Complete"}
                {file.status === "uploading" && `${file.progress}%`}
                {file.status === "queued" && "Queued"}
              </span>
            </div>
            <ProgressBar
              current={file.progress}
              total={100}
              size="xs"
              status={
                file.status === "complete"
                  ? "success"
                  : file.status === "uploading"
                    ? "active"
                    : "idle"
              }
              progressType="file_progress"
            />
          </div>
        ))}
      </div>
    </div>
  );
}

export const FileUploadExample: Story = {
  render: () => <FileUploadExampleComponent />,
  parameters: {
    docs: {
      description: {
        story: "Real-world example: File upload progress with animated updates",
      },
    },
  },
};

// ===========================================
// Accessibility Demo
// ===========================================

export const AccessibilityDemo: Story = {
  render: () => {
    return (
      <div className="space-y-6 w-[400px]">
        <h2 className="text-lg font-semibold">Accessibility Features</h2>

        <div className="space-y-4">
          <div className="p-4 bg-muted/20 rounded-lg">
            <h3 className="text-sm font-medium mb-2">ARIA Attributes</h3>
            <p className="text-xs text-muted-foreground mb-3">
              All progress bars include proper ARIA attributes:
            </p>
            <code className="text-xs block bg-muted/50 p-2 rounded">
              {`role="progressbar"
aria-valuenow={current}
aria-valuemin={0}
aria-valuemax={total}
aria-label="Progress: 50/100"
aria-busy={status === "active"}`}
            </code>
          </div>

          <div className="p-4 bg-muted/20 rounded-lg">
            <h3 className="text-sm font-medium mb-2">Custom ARIA Label</h3>
            <ProgressBar
              current={50}
              total={100}
              showLabel
              labelFormat="count"
              ariaLabel="Uploading files: 50 of 100 complete"
            />
            <p className="text-xs text-muted-foreground mt-2">
              Use <code>ariaLabel</code> prop for custom accessibility labels
            </p>
          </div>

          <div className="p-4 bg-muted/20 rounded-lg">
            <h3 className="text-sm font-medium mb-2">Indeterminate State</h3>
            <ProgressBar current={42} indeterminate showLabel labelFormat="count" />
            <p className="text-xs text-muted-foreground mt-2">
              Indeterminate progress has <code>aria-valuenow</code> undefined
            </p>
          </div>
        </div>
      </div>
    );
  },
  parameters: {
    docs: {
      description: {
        story: "Demonstrates accessibility features including ARIA attributes",
      },
    },
  },
};
