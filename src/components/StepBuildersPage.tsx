/**
 * StepBuildersPage.tsx
 *
 * Landing page showing a card grid of available step builder types.
 * Each card navigates to the corresponding builder page when clicked.
 */

import {
  Layers,
  CheckCircle2,
  ListChecks,
  Terminal,
  Zap,
  Code,
  FlaskConical,
  ArrowRight,
} from "lucide-react";

interface StepBuildersPageProps {
  onNavigate: (builderId: string) => void;
}

interface BuilderCard {
  id: string;
  title: string;
  description: string;
  icon: React.ComponentType<{ className?: string }>;
  colorClasses: {
    bg: string;
    text: string;
  };
}

const BUILDER_CARDS: BuilderCard[] = [
  {
    id: "check-builder",
    title: "Checks",
    description: "Verification checks for validating UI state",
    icon: CheckCircle2,
    colorClasses: {
      bg: "bg-emerald-500/15",
      text: "text-emerald-500",
    },
  },
  {
    id: "check-group-builder",
    title: "Check Groups",
    description: "Group checks into reusable test suites",
    icon: ListChecks,
    colorClasses: {
      bg: "bg-green-500/15",
      text: "text-green-500",
    },
  },
  {
    id: "shell-command-builder",
    title: "Shell Commands",
    description: "System commands and shell scripts",
    icon: Terminal,
    colorClasses: {
      bg: "bg-orange-500/15",
      text: "text-orange-500",
    },
  },
  {
    id: "task-builder",
    title: "Tasks",
    description: "Automation task definitions",
    icon: Zap,
    colorClasses: {
      bg: "bg-purple-500/15",
      text: "text-purple-500",
    },
  },
  {
    id: "context-builder",
    title: "Contexts",
    description: "Reusable context configurations",
    icon: Code,
    colorClasses: {
      bg: "bg-violet-500/15",
      text: "text-violet-500",
    },
  },
  {
    id: "playwright-test-builder",
    title: "Playwright Tests",
    description: "Browser automation tests",
    icon: FlaskConical,
    colorClasses: {
      bg: "bg-blue-500/15",
      text: "text-blue-500",
    },
  },
];

export function StepBuildersPage({ onNavigate }: StepBuildersPageProps) {
  return (
    <div className="h-full overflow-y-auto p-6">
      {/* Header */}
      <div className="mb-6">
        <div className="flex items-center gap-3 mb-2">
          <div className="w-10 h-10 rounded-lg bg-primary/15 flex items-center justify-center">
            <Layers className="w-5 h-5 text-primary" />
          </div>
          <div>
            <h1 className="text-xl font-semibold text-foreground">Step Builders</h1>
            <p className="text-sm text-muted-foreground">
              Build individual workflow steps with specialized editors
            </p>
          </div>
        </div>
      </div>

      {/* Card Grid */}
      <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-3">
        {BUILDER_CARDS.map((card) => {
          const Icon = card.icon;
          return (
            <button
              key={card.id}
              onClick={() => onNavigate(card.id)}
              className="group flex items-start gap-4 p-4 rounded-lg border border-border bg-card hover:bg-accent/50 transition-colors text-left"
            >
              <div
                className={`w-10 h-10 rounded-lg flex-shrink-0 flex items-center justify-center ${card.colorClasses.bg}`}
              >
                <Icon className={`w-5 h-5 ${card.colorClasses.text}`} />
              </div>
              <div className="flex-1 min-w-0">
                <div className="flex items-center justify-between">
                  <h3 className="text-sm font-medium text-foreground">{card.title}</h3>
                  <ArrowRight className="w-4 h-4 text-muted-foreground opacity-0 group-hover:opacity-100 transition-opacity" />
                </div>
                <p className="text-xs text-muted-foreground mt-1">{card.description}</p>
              </div>
            </button>
          );
        })}
      </div>
    </div>
  );
}
