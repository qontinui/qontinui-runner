/**
 * HelpTab.tsx
 *
 * Help tab component with sub-pages using vertical tabs:
 * - Shortcuts: Keyboard shortcuts and quick actions
 * - Getting Started: Quick overview and first steps
 * - Documentation: Links to external documentation
 * - Troubleshooting: Common issues and solutions
 * - About: Version info, credits, and links
 */

import { useState, useEffect } from "react";
import * as Tabs from "@radix-ui/react-tabs";
import { instanceStorage } from "@/lib/instance-storage";
import {
  Keyboard,
  HelpCircle,
  Rocket,
  BookOpen,
  Wrench,
  Info,
  ExternalLink,
  CheckCircle2,
  Circle,
  ChevronDown,
  ChevronRight,
  GitFork,
  Bug,
  FileText,
  Zap,
  Settings,
  Play,
  FolderOpen,
  Bot,
  GraduationCap,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { getAccentColors, getStatusColors } from "@/design-system";
import { useTutorial, useTutorialProgress } from "../contexts/TutorialContext";
import { TutorialCard } from "./tutorial/TutorialCard";
import { TutorialTrigger } from "./tutorial";
import { tutorials, getFeaturedTutorials } from "./tutorial/data";
import { Star, MapPin } from "lucide-react";
import type { ProductTour, TourAudience } from "@/types/product-tour";
import { PRODUCT_TOURS_STORAGE_KEY } from "@/types/product-tour";
import { TourCard } from "./product-tour/TourCard";
import { TourPlayer } from "./product-tour/TourPlayer";
import { exportTour } from "@/lib/product-tour/tour-exporter";

type HelpSubPage =
  | "tutorials"
  | "product-tours"
  | "shortcuts"
  | "getting-started"
  | "documentation"
  | "troubleshooting"
  | "about";

const STORAGE_KEY = "qontinui-help-active-tab";

interface ShortcutItem {
  keys: string[];
  description: string;
  context: string;
}

interface ShortcutCategory {
  name: string;
  shortcuts: ShortcutItem[];
}

const SHORTCUTS: ShortcutCategory[] = [
  {
    name: "Playwright Test Builder",
    shortcuts: [
      {
        keys: ["@"],
        description: "Open prompt snippet selector popup",
        context:
          "In the Description field, type @ to search and insert a prompt snippet at the cursor position",
      },
    ],
  },
  {
    name: "Prompt Snippet Selector",
    shortcuts: [
      {
        keys: ["\u2191", "\u2193"],
        description: "Navigate through prompt snippet list",
        context: "When prompt snippet selector popup is open",
      },
      {
        keys: ["Enter"],
        description: "Insert selected prompt snippet",
        context: "When prompt snippet selector popup is open",
      },
      {
        keys: ["Escape"],
        description: "Close prompt snippet selector",
        context: "When prompt snippet selector popup is open",
      },
    ],
  },
];

interface TroubleshootingItem {
  title: string;
  description: string;
  solution: string[];
}

const TROUBLESHOOTING_ITEMS: TroubleshootingItem[] = [
  {
    title: "Code check fails to run",
    description: "A lint, format, or type check fails to execute.",
    solution: [
      "Verify the tool is installed (e.g., npm install -g eslint, pip install ruff)",
      "Check that the working directory is correct",
      "Ensure any config files (eslintrc, pyproject.toml) are valid",
      "Try running the command manually in a terminal to see the full error",
      "Check the timeout setting - type checks can take longer",
    ],
  },
  {
    title: "Python executor won't start",
    description: "The executor shows as stopped or fails to initialize.",
    solution: [
      "Check that Python 3.10+ is installed and available in PATH",
      "Verify the virtual environment is properly configured in Settings > Debug",
      "Check the Logs tab for specific error messages",
      "Try restarting the runner application",
    ],
  },
  {
    title: "Workflow won't load",
    description: "Loading or importing a workflow fails or shows errors.",
    solution: [
      "Verify the workflow file is valid JSON (use a linter)",
      "Check that the workflow follows the expected schema",
      "Ensure all referenced scripts and contexts exist",
      "Look for syntax errors like missing commas or brackets",
    ],
  },
  {
    title: "AI analysis not working",
    description: "The AI analysis feature fails or returns empty results.",
    solution: [
      "Verify your AI provider API key is configured in Settings > AI Providers",
      "Check your internet connection",
      "Ensure you have sufficient API credits/quota",
      "Try switching to a different AI provider or model",
    ],
  },
  {
    title: "SDK app not connecting",
    description: "An SDK-integrated app won't connect to the runner via UI Bridge.",
    solution: [
      "Check that the runner is running (its HTTP API port must be active)",
      "Verify the SDK app is running and has the UI Bridge SDK initialized",
      "Check the SDK app console for connection errors",
      "Try refreshing the SDK app and reconnecting",
    ],
  },
  {
    title: "Workflow execution hangs",
    description: "A workflow starts but never completes or times out.",
    solution: [
      "Check the AI Output tab for any error messages",
      "Verify the AI provider API key is valid and has quota",
      "Increase the max sessions or timeout values in the workflow",
      "Use the Stop button to halt execution and review the session summary",
    ],
  },
];

interface DocLink {
  title: string;
  description: string;
  url: string;
  icon: React.ElementType;
}

const DOCUMENTATION_LINKS: DocLink[] = [
  {
    title: "Getting Started Guide",
    description: "Learn the basics of AI-assisted development workflows",
    url: "https://github.com/qontinui/qontinui-runner#getting-started",
    icon: Rocket,
  },
  {
    title: "Configuration Reference",
    description: "Complete reference for workflow and AI session configuration",
    url: "https://github.com/qontinui/qontinui-runner/wiki",
    icon: FileText,
  },
  {
    title: "API Documentation",
    description: "MCP API and runner integration documentation",
    url: "https://github.com/qontinui/qontinui-runner#api",
    icon: Zap,
  },
  {
    title: "Report Issues",
    description: "Found a bug? Let us know on GitHub",
    url: "https://github.com/qontinui/qontinui-runner/issues",
    icon: Bug,
  },
];

function ShortcutsPage() {
  return (
    <div className="space-y-6">
      <div className="flex items-center gap-3">
        <div className="p-2 bg-primary/10 rounded-lg">
          <Keyboard className="w-5 h-5 text-primary" />
        </div>
        <div>
          <h2 className="text-lg font-semibold">Keyboard Shortcuts</h2>
          <p className="text-sm text-muted-foreground">
            Quick actions and keyboard shortcuts to speed up your workflow
          </p>
        </div>
      </div>

      <div className="space-y-6">
        {SHORTCUTS.map((category) => (
          <div key={category.name} className="space-y-3">
            <h3 className="text-sm font-medium text-muted-foreground uppercase tracking-wider">
              {category.name}
            </h3>
            <div className="bg-card border border-border rounded-lg divide-y divide-border">
              {category.shortcuts.map((shortcut, idx) => (
                <div
                  key={`${shortcut.keys.join("-")}-${idx}`}
                  className="p-4 flex items-start gap-4"
                >
                  <div className="flex items-center gap-1.5 shrink-0">
                    {shortcut.keys.map((key, keyIdx) => (
                      <span key={`${key}-${keyIdx}`}>
                        <kbd className="px-2 py-1 text-sm font-mono bg-muted border border-border rounded shadow-xs">
                          {key}
                        </kbd>
                        {keyIdx < shortcut.keys.length - 1 && (
                          <span className="mx-1 text-muted-foreground">+</span>
                        )}
                      </span>
                    ))}
                  </div>
                  <div className="flex-1 min-w-0">
                    <div className="font-medium">{shortcut.description}</div>
                    <div className="text-sm text-muted-foreground mt-0.5">{shortcut.context}</div>
                  </div>
                </div>
              ))}
            </div>
          </div>
        ))}
      </div>

      {/* Tips section */}
      <div
        className={`${getAccentColors("blue").bg} border ${getAccentColors("blue").border} rounded-lg p-4`}
      >
        <div className="flex items-start gap-3">
          <HelpCircle className={`w-5 h-5 ${getAccentColors("blue").text} shrink-0 mt-0.5`} />
          <div>
            <h4 className={`font-medium ${getAccentColors("blue").text}`}>
              Tip: Using Prompt Snippets
            </h4>
            <p className="text-sm text-muted-foreground mt-1">
              Prompt snippets are reusable text snippets that capture learnings from AI debugging
              sessions. Create them in the <strong>Prompt Snippets</strong> tab, then insert them
              into test descriptions using the dropdown button or by typing{" "}
              <kbd className="px-1.5 py-0.5 text-xs bg-muted border border-border rounded">@</kbd>{" "}
              in the description field.
            </p>
          </div>
        </div>
      </div>
    </div>
  );
}

interface ChecklistItemProps {
  label: string;
  description: string;
  icon: React.ElementType;
  completed?: boolean;
}

function ChecklistItem({ label, description, icon: Icon, completed = false }: ChecklistItemProps) {
  return (
    <div className="flex items-start gap-3 p-3 rounded-lg hover:bg-muted/50 transition-colors">
      <div className="mt-0.5">
        {completed ? (
          <CheckCircle2 className={`w-5 h-5 ${getStatusColors("success").icon}`} />
        ) : (
          <Circle className="w-5 h-5 text-muted-foreground" />
        )}
      </div>
      <div className="flex-1">
        <div className="flex items-center gap-2">
          <Icon className="w-4 h-4 text-primary" />
          <span className="font-medium">{label}</span>
        </div>
        <p className="text-sm text-muted-foreground mt-0.5">{description}</p>
      </div>
    </div>
  );
}

function GettingStartedPage() {
  return (
    <div className="space-y-6">
      <div className="flex items-center gap-3">
        <div className="p-2 bg-primary/10 rounded-lg">
          <Rocket className="w-5 h-5 text-primary" />
        </div>
        <div>
          <h2 className="text-lg font-semibold">Getting Started</h2>
          <p className="text-sm text-muted-foreground">
            Welcome to Qontinui Runner - your desktop automation companion
          </p>
        </div>
      </div>

      {/* Overview Card */}
      <div className="bg-card rounded-lg border border-border p-5">
        <h3 className="font-semibold mb-3">What is Qontinui Runner?</h3>
        <p className="text-sm text-muted-foreground leading-relaxed">
          Qontinui Runner is a desktop application for AI-assisted development with visual feedback
          loops. It orchestrates AI sessions that can verify their own work, detect errors
          automatically, and iterate toward goals with persistent knowledge across sessions.
        </p>
      </div>

      {/* Key Features */}
      <div className="bg-card rounded-lg border border-border p-5">
        <h3 className="font-semibold mb-4">Key Features</h3>
        <div className="grid gap-3">
          <div className="flex items-start gap-3">
            <div className="p-1.5 bg-primary/10 rounded">
              <Zap className="w-4 h-4 text-primary" />
            </div>
            <div>
              <div className="font-medium">UI Bridge Feedback</div>
              <p className="text-sm text-muted-foreground">
                Get real-time visual feedback from your application via the UI Bridge
              </p>
            </div>
          </div>
          <div className="flex items-start gap-3">
            <div className="p-1.5 bg-primary/10 rounded">
              <Bot className="w-4 h-4 text-primary" />
            </div>
            <div>
              <div className="font-medium">AI-Powered Analysis</div>
              <p className="text-sm text-muted-foreground">
                Use Claude or other AI providers to analyze screens and make decisions
              </p>
            </div>
          </div>
          <div className="flex items-start gap-3">
            <div className="p-1.5 bg-primary/10 rounded">
              <Play className="w-4 h-4 text-primary" />
            </div>
            <div>
              <div className="font-medium">Orchestrated Workflows</div>
              <p className="text-sm text-muted-foreground">
                Run multi-phase workflows with verification loops and self-healing
              </p>
            </div>
          </div>
          <div className="flex items-start gap-3">
            <div className="p-1.5 bg-primary/10 rounded">
              <Wrench className="w-4 h-4 text-primary" />
            </div>
            <div>
              <div className="font-medium">Code Quality Checks</div>
              <p className="text-sm text-muted-foreground">
                Run linters, formatters, and type checkers with auto-fix support
              </p>
            </div>
          </div>
        </div>
      </div>

      {/* First Steps Checklist */}
      <div className="bg-card rounded-lg border border-border p-5">
        <h3 className="font-semibold mb-4">First Steps</h3>
        <div className="space-y-1">
          <ChecklistItem
            icon={Settings}
            label="Configure AI Provider"
            description="Set up your Claude or other AI provider API key in Settings"
          />
          <ChecklistItem
            icon={FolderOpen}
            label="Set Working Directory"
            description="Point the runner at your project's codebase"
          />
          <ChecklistItem
            icon={ExternalLink}
            label="Integrate UI Bridge SDK"
            description="Enable UI Bridge feedback from your target application"
          />
          <ChecklistItem
            icon={Play}
            label="Create Your First Workflow"
            description="Build a workflow with AI-powered setup, verification, and agentic steps"
          />
        </div>
      </div>

      {/* Tip */}
      <div
        className={`${getAccentColors("blue").bg} border ${getAccentColors("blue").border} rounded-lg p-4`}
      >
        <div className="flex items-start gap-3">
          <HelpCircle className={`w-5 h-5 ${getAccentColors("blue").text} shrink-0 mt-0.5`} />
          <div>
            <h4 className={`font-medium ${getAccentColors("blue").text}`}>Need Help?</h4>
            <p className="text-sm text-muted-foreground mt-1">
              Check the <strong>Troubleshooting</strong> section for common issues, or visit the{" "}
              <strong>Documentation</strong> for in-depth guides.
            </p>
          </div>
        </div>
      </div>
    </div>
  );
}

function DocumentationPage() {
  return (
    <div className="space-y-6">
      <div className="flex items-center gap-3">
        <div className="p-2 bg-primary/10 rounded-lg">
          <BookOpen className="w-5 h-5 text-primary" />
        </div>
        <div>
          <h2 className="text-lg font-semibold">Documentation</h2>
          <p className="text-sm text-muted-foreground">
            Resources and guides to help you get the most out of Qontinui
          </p>
        </div>
      </div>

      {/* Documentation Links */}
      <div className="grid gap-3">
        {DOCUMENTATION_LINKS.map((link) => {
          const Icon = link.icon;
          return (
            <a
              key={link.title}
              href={link.url}
              target="_blank"
              rel="noopener noreferrer"
              className="bg-card rounded-lg border border-border p-4 hover:border-primary/50 hover:bg-muted/30 transition-colors group"
            >
              <div className="flex items-start gap-3">
                <div className="p-2 bg-primary/10 rounded-lg">
                  <Icon className="w-4 h-4 text-primary" />
                </div>
                <div className="flex-1">
                  <div className="flex items-center gap-2">
                    <span className="font-medium group-hover:text-primary transition-colors">
                      {link.title}
                    </span>
                    <ExternalLink className="w-3.5 h-3.5 text-muted-foreground opacity-0 group-hover:opacity-100 transition-opacity" />
                  </div>
                  <p className="text-sm text-muted-foreground mt-1">{link.description}</p>
                </div>
              </div>
            </a>
          );
        })}
      </div>

      {/* Additional Resources */}
      <div className="bg-card rounded-lg border border-border p-5">
        <h3 className="font-semibold mb-4">Additional Resources</h3>
        <div className="space-y-3">
          <a
            href="https://github.com/qontinui/qontinui-runner"
            target="_blank"
            rel="noopener noreferrer"
            className="flex items-center gap-3 text-sm hover:text-primary transition-colors"
          >
            <GitFork className="w-4 h-4" />
            <span>GitHub Repository</span>
            <ExternalLink className="w-3 h-3 text-muted-foreground" />
          </a>
          <a
            href="https://github.com/qontinui/qontinui-runner/discussions"
            target="_blank"
            rel="noopener noreferrer"
            className="flex items-center gap-3 text-sm hover:text-primary transition-colors"
          >
            <HelpCircle className="w-4 h-4" />
            <span>Community Discussions</span>
            <ExternalLink className="w-3 h-3 text-muted-foreground" />
          </a>
          <a
            href="https://github.com/qontinui/qontinui-runner/releases"
            target="_blank"
            rel="noopener noreferrer"
            className="flex items-center gap-3 text-sm hover:text-primary transition-colors"
          >
            <FileText className="w-4 h-4" />
            <span>Release Notes</span>
            <ExternalLink className="w-3 h-3 text-muted-foreground" />
          </a>
        </div>
      </div>
    </div>
  );
}

interface TroubleshootingItemComponentProps {
  item: TroubleshootingItem;
}

function TroubleshootingItemComponent({ item }: TroubleshootingItemComponentProps) {
  const [isOpen, setIsOpen] = useState(false);

  return (
    <div className="bg-card rounded-lg border border-border overflow-hidden">
      <button
        onClick={() => setIsOpen(!isOpen)}
        className="w-full p-4 flex items-start gap-3 text-left hover:bg-muted/30 transition-colors"
      >
        <div className="mt-0.5">
          {isOpen ? (
            <ChevronDown className="w-4 h-4 text-muted-foreground" />
          ) : (
            <ChevronRight className="w-4 h-4 text-muted-foreground" />
          )}
        </div>
        <div className="flex-1">
          <div className="font-medium">{item.title}</div>
          <p className="text-sm text-muted-foreground mt-0.5">{item.description}</p>
        </div>
      </button>
      {isOpen && (
        <div className="px-4 pb-4 pt-0 ml-7">
          <div className="bg-muted/50 rounded-lg p-4">
            <h4 className="text-sm font-medium mb-2">Solution:</h4>
            <ul className="space-y-2">
              {item.solution.map((step, idx) => (
                <li
                  key={`step-${idx}-${step.slice(0, 20)}`}
                  className="flex items-start gap-2 text-sm text-muted-foreground"
                >
                  <span className="text-primary font-medium">{idx + 1}.</span>
                  <span>{step}</span>
                </li>
              ))}
            </ul>
          </div>
        </div>
      )}
    </div>
  );
}

function TroubleshootingPage() {
  return (
    <div className="space-y-6">
      <div className="flex items-center gap-3">
        <div className="p-2 bg-primary/10 rounded-lg">
          <Wrench className="w-5 h-5 text-primary" />
        </div>
        <div>
          <h2 className="text-lg font-semibold">Troubleshooting</h2>
          <p className="text-sm text-muted-foreground">Common issues and their solutions</p>
        </div>
      </div>

      {/* Troubleshooting Items */}
      <div className="space-y-3">
        {TROUBLESHOOTING_ITEMS.map((item, idx) => (
          <TroubleshootingItemComponent key={`${item.title.slice(0, 20)}-${idx}`} item={item} />
        ))}
      </div>

      {/* Still Need Help */}
      <div
        className={`${getAccentColors("amber").bg} border ${getAccentColors("amber").border} rounded-lg p-4`}
      >
        <div className="flex items-start gap-3">
          <Bug className={`w-5 h-5 ${getAccentColors("amber").text} shrink-0 mt-0.5`} />
          <div>
            <h4 className={`font-medium ${getAccentColors("amber").text}`}>Still Having Issues?</h4>
            <p className="text-sm text-muted-foreground mt-1">
              If your issue isn't listed here, check the Logs tab for detailed error messages or{" "}
              <a
                href="https://github.com/qontinui/qontinui-runner/issues/new"
                target="_blank"
                rel="noopener noreferrer"
                className="text-primary hover:underline"
              >
                report a bug on GitHub
              </a>
              .
            </p>
          </div>
        </div>
      </div>
    </div>
  );
}

function TutorialsPage() {
  const { openTutorial, resetProgress } = useTutorial();
  const { isCompleted, isInProgress } = useTutorialProgress();

  const featuredTutorials = getFeaturedTutorials();
  const regularTutorials = tutorials.filter((t) => !t.tags?.includes("featured"));

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <div className="p-2 bg-primary/10 rounded-lg">
            <GraduationCap className="w-5 h-5 text-primary" />
          </div>
          <div>
            <h2 className="text-lg font-semibold">Interactive Tutorials</h2>
            <p className="text-sm text-muted-foreground">
              Step-by-step guides to help you master Qontinui Runner
            </p>
          </div>
        </div>
        <button
          onClick={resetProgress}
          className="text-xs text-muted-foreground hover:text-foreground transition-colors"
        >
          Reset progress
        </button>
      </div>

      {/* Featured tutorial - highlighted section */}
      {featuredTutorials.length > 0 && (
        <div className="space-y-3">
          <div className="flex items-center gap-2">
            <Star className={`w-4 h-4 ${getAccentColors("amber").text}`} />
            <h3 className="text-sm font-medium">Featured</h3>
          </div>
          <div
            className={`p-1 rounded-xl bg-gradient-to-r ${getAccentColors("amber").bg} border ${getAccentColors("amber").border}`}
          >
            <div className="bg-card rounded-lg">
              {featuredTutorials.map((tutorial) => (
                <TutorialCard
                  key={tutorial.id}
                  tutorial={tutorial}
                  isCompleted={isCompleted(tutorial.id)}
                  isInProgress={isInProgress(tutorial.id)}
                  onStart={() => openTutorial(tutorial)}
                  onResume={() => openTutorial(tutorial)}
                  onRestart={() => openTutorial(tutorial)}
                  featured
                />
              ))}
            </div>
          </div>
        </div>
      )}

      {/* Regular tutorial list */}
      <div className="space-y-3">
        <h3 className="text-sm font-medium text-muted-foreground">All Tutorials</h3>
        <div className="space-y-4">
          {regularTutorials.map((tutorial) => (
            <TutorialCard
              key={tutorial.id}
              tutorial={tutorial}
              isCompleted={isCompleted(tutorial.id)}
              isInProgress={isInProgress(tutorial.id)}
              onStart={() => openTutorial(tutorial)}
              onResume={() => openTutorial(tutorial)}
              onRestart={() => openTutorial(tutorial)}
            />
          ))}
        </div>
      </div>

      {/* Tips */}
      <div
        className={`${getAccentColors("blue").bg} border ${getAccentColors("blue").border} rounded-lg p-4`}
      >
        <div className="flex items-start gap-3">
          <HelpCircle className={`w-5 h-5 ${getAccentColors("blue").text} shrink-0 mt-0.5`} />
          <div>
            <h4 className={`font-medium ${getAccentColors("blue").text}`}>
              Tip: Keyboard Navigation
            </h4>
            <p className="text-sm text-muted-foreground mt-1">
              Use the arrow keys to navigate between tutorial steps. Press{" "}
              <kbd className="px-1.5 py-0.5 text-xs bg-muted border border-border rounded">
                Escape
              </kbd>{" "}
              to close the tutorial at any time.
            </p>
          </div>
        </div>
      </div>
    </div>
  );
}

// =============================================================================
// Product Tours Page
// =============================================================================

const AUDIENCE_LABELS: Record<TourAudience, string> = {
  "new-user": "Getting Started",
  "power-user": "Power Features",
  evaluator: "Product Overview",
};

function ProductToursPage() {
  const [tours] = useState<ProductTour[]>(
    () => instanceStorage.getJSON<ProductTour[]>(PRODUCT_TOURS_STORAGE_KEY, []),
  );
  const [activeTour, setActiveTour] = useState<ProductTour | null>(null);

  const handleExport = (tour: ProductTour, format: "html" | "json" | "markdown") => {
    const exports = exportTour(tour);
    const formatMap = {
      html: { content: exports.html, mime: "text/html", ext: "html" },
      json: { content: exports.json, mime: "application/json", ext: "json" },
      markdown: { content: exports.markdown, mime: "text/markdown", ext: "md" },
    };
    const { content, mime, ext } = formatMap[format];
    const blob = new Blob([content], { type: mime });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `${tour.id}.${ext}`;
    a.click();
    URL.revokeObjectURL(url);
  };

  const toursByAudience = (aud: TourAudience) => tours.filter((t) => t.targetAudience === aud);

  if (tours.length === 0) {
    return (
      <div className="space-y-4">
        <div className="flex items-center gap-3">
          <div className="p-2 bg-primary/10 rounded-lg">
            <MapPin className="w-5 h-5 text-primary" />
          </div>
          <div>
            <h2 className="text-lg font-semibold">Product Tours</h2>
            <p className="text-sm text-muted-foreground">
              Interactive tours that demonstrate features automatically
            </p>
          </div>
        </div>
        <p className="text-sm text-muted-foreground">
          No tours generated yet. Go to <strong>Build &gt; Product Tours</strong> to generate tours from page specs.
        </p>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-3">
        <div className="p-2 bg-primary/10 rounded-lg">
          <MapPin className="w-5 h-5 text-primary" />
        </div>
        <div>
          <h2 className="text-lg font-semibold">Product Tours</h2>
          <p className="text-sm text-muted-foreground">
            Interactive tours that demonstrate features automatically
          </p>
        </div>
      </div>

      {(["new-user", "power-user", "evaluator"] as TourAudience[]).map((aud) => {
        const audienceTours = toursByAudience(aud);
        if (audienceTours.length === 0) return null;
        return (
          <div key={aud} className="space-y-3">
            <h3 className="text-sm font-medium text-muted-foreground">
              {AUDIENCE_LABELS[aud]}
            </h3>
            <div className="grid gap-3 sm:grid-cols-2">
              {audienceTours.map((tour) => (
                <TourCard
                  key={tour.id}
                  tour={tour}
                  onLaunch={setActiveTour}
                  onExport={handleExport}
                />
              ))}
            </div>
          </div>
        );
      })}

      {activeTour && (
        <TourPlayer
          tour={activeTour}
          onClose={() => setActiveTour(null)}
          onComplete={() => setActiveTour(null)}
        />
      )}
    </div>
  );
}

interface VersionInfo {
  current_version: string;
}

function AboutPage() {
  const [version, setVersion] = useState<string>("1.0.0");
  const [buildDate] = useState<string>(() => {
    // In a real app, this would come from build-time constants
    return new Date().toLocaleDateString("en-US", {
      year: "numeric",
      month: "long",
      day: "numeric",
    });
  });

  useEffect(() => {
    // Try to get version from update info
    const fetchVersion = async () => {
      try {
        const result = (await invoke("check_for_updates")) as {
          success: boolean;
          data?: VersionInfo;
        };
        if (result.success && result.data?.current_version) {
          setVersion(result.data.current_version);
        }
      } catch {
        // Fallback to default version
      }
    };
    fetchVersion();
  }, []);

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-3">
        <div className="p-2 bg-primary/10 rounded-lg">
          <Info className="w-5 h-5 text-primary" />
        </div>
        <div>
          <h2 className="text-lg font-semibold">About Qontinui Runner</h2>
          <p className="text-sm text-muted-foreground">Version information and credits</p>
        </div>
      </div>

      {/* Version Card */}
      <div className="bg-card rounded-lg border border-border p-5">
        <div className="flex items-center justify-between">
          <div>
            <h3 className="font-semibold">Qontinui Runner</h3>
            <p className="text-sm text-muted-foreground mt-1">AI-assisted development tool</p>
          </div>
          <div className="text-right">
            <div className="text-2xl font-bold text-primary">v{version}</div>
            <div className="text-xs text-muted-foreground mt-1">{buildDate}</div>
          </div>
        </div>
      </div>

      {/* Links */}
      <div className="bg-card rounded-lg border border-border p-5">
        <h3 className="font-semibold mb-4">Links</h3>
        <div className="space-y-3">
          <a
            href="https://github.com/qontinui/qontinui-runner"
            target="_blank"
            rel="noopener noreferrer"
            className="flex items-center gap-3 text-sm hover:text-primary transition-colors"
          >
            <GitFork className="w-4 h-4" />
            <span>GitHub Repository</span>
            <ExternalLink className="w-3 h-3 text-muted-foreground" />
          </a>
          <a
            href="https://github.com/qontinui/qontinui-runner/issues/new"
            target="_blank"
            rel="noopener noreferrer"
            className="flex items-center gap-3 text-sm hover:text-primary transition-colors"
          >
            <Bug className="w-4 h-4" />
            <span>Report an Issue</span>
            <ExternalLink className="w-3 h-3 text-muted-foreground" />
          </a>
          <a
            href="https://github.com/qontinui/qontinui-runner/blob/main/CHANGELOG.md"
            target="_blank"
            rel="noopener noreferrer"
            className="flex items-center gap-3 text-sm hover:text-primary transition-colors"
          >
            <FileText className="w-4 h-4" />
            <span>Changelog</span>
            <ExternalLink className="w-3 h-3 text-muted-foreground" />
          </a>
        </div>
      </div>

      {/* Credits */}
      <div className="bg-card rounded-lg border border-border p-5">
        <h3 className="font-semibold mb-4">Credits</h3>
        <div className="text-sm text-muted-foreground space-y-2">
          <p>
            <strong className="text-foreground">Developed by:</strong> Joshua Spinak and the
            Qontinui team
          </p>
          <p>
            <strong className="text-foreground">Built with:</strong> Tauri, React, TypeScript, and
            Rust
          </p>
          <p>
            <strong className="text-foreground">License:</strong> MIT
          </p>
        </div>
      </div>

      {/* Open Source Notice */}
      <div className="bg-primary/5 border border-primary/20 rounded-lg p-4">
        <div className="flex items-start gap-3">
          <HelpCircle className="w-5 h-5 text-primary shrink-0 mt-0.5" />
          <div>
            <h4 className="font-medium text-primary">Open Source</h4>
            <p className="text-sm text-muted-foreground mt-1">
              Qontinui Runner is open source software. Contributions, bug reports, and feature
              requests are welcome on GitHub.
            </p>
          </div>
        </div>
      </div>
    </div>
  );
}

export function HelpTab() {
  const [activeSubPage, setActiveSubPage] = useState<HelpSubPage>(() => {
    // Load persisted tab on mount
    const stored = instanceStorage.getItem(STORAGE_KEY);
    if (
      stored &&
      [
        "tutorials",
        "shortcuts",
        "getting-started",
        "documentation",
        "troubleshooting",
        "about",
      ].includes(stored)
    ) {
      return stored as HelpSubPage;
    }
    return "tutorials";
  });

  // Persist active tab
  useEffect(() => {
    instanceStorage.setItem(STORAGE_KEY, activeSubPage);
  }, [activeSubPage]);

  const subPages = [
    { id: "tutorials" as const, label: "Tutorials", icon: GraduationCap },
    { id: "product-tours" as const, label: "Product Tours", icon: MapPin },
    { id: "shortcuts" as const, label: "Shortcuts", icon: Keyboard },
    { id: "getting-started" as const, label: "Getting Started", icon: Rocket },
    { id: "documentation" as const, label: "Documentation", icon: BookOpen },
    { id: "troubleshooting" as const, label: "Troubleshooting", icon: Wrench },
    { id: "about" as const, label: "About", icon: Info },
  ];

  return (
    <Tabs.Root
      value={activeSubPage}
      onValueChange={(value) => setActiveSubPage(value as HelpSubPage)}
      orientation="vertical"
      className="flex h-full min-h-[500px]"
    >
      {/* Left sidebar with tabs */}
      <Tabs.List className="flex flex-col w-44 shrink-0 border-r border-border/50 bg-card/50 p-2 gap-1">
        <div className="px-3 py-2 mb-2">
          <div className="flex items-center justify-between">
            <h3 className="text-lg font-semibold flex items-center gap-2">
              <HelpCircle className="w-5 h-5 text-primary" />
              Help
            </h3>
            <TutorialTrigger
              tutorialId="getting-started"
              tooltip="Start the getting started tutorial"
              icon="lightbulb"
              size="sm"
            />
          </div>
        </div>

        {subPages.map((page) => {
          const Icon = page.icon;
          return (
            <Tabs.Trigger
              key={page.id}
              value={page.id}
              className={`
                flex items-center gap-3 px-3 py-2.5 rounded-md text-left text-sm font-medium
                transition-colors duration-150 outline-hidden
                data-[state=active]:bg-primary data-[state=active]:text-primary-foreground
                data-[state=inactive]:text-muted-foreground data-[state=inactive]:hover:bg-muted/50
                focus-visible:ring-2 focus-visible:ring-primary focus-visible:ring-offset-2
              `}
            >
              <Icon className="w-4 h-4 shrink-0" />
              <span>{page.label}</span>
            </Tabs.Trigger>
          );
        })}

        {/* Spacer to push help text to bottom */}
        <div className="flex-1" />

        {/* Help text at bottom */}
        <div className="px-3 py-2 text-xs text-muted-foreground border-t border-border/50 mt-2 pt-3">
          Use arrow keys to navigate between tabs
        </div>
      </Tabs.List>

      {/* Content area */}
      <div className="flex-1 overflow-y-auto p-6">
        <Tabs.Content value="tutorials" className="outline-hidden">
          <TutorialsPage />
        </Tabs.Content>

        <Tabs.Content value="product-tours" className="outline-hidden">
          <ProductToursPage />
        </Tabs.Content>

        <Tabs.Content value="shortcuts" className="outline-hidden">
          <ShortcutsPage />
        </Tabs.Content>

        <Tabs.Content value="getting-started" className="outline-hidden">
          <GettingStartedPage />
        </Tabs.Content>

        <Tabs.Content value="documentation" className="outline-hidden">
          <DocumentationPage />
        </Tabs.Content>

        <Tabs.Content value="troubleshooting" className="outline-hidden">
          <TroubleshootingPage />
        </Tabs.Content>

        <Tabs.Content value="about" className="outline-hidden">
          <AboutPage />
        </Tabs.Content>
      </div>
    </Tabs.Root>
  );
}
