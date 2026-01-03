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
  Github,
  Bug,
  FileText,
  Zap,
  Settings,
  Play,
  FolderOpen,
  Bot,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";

type HelpSubPage = "shortcuts" | "getting-started" | "documentation" | "troubleshooting" | "about";

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
    name: "Script Builder",
    shortcuts: [
      {
        keys: ["@"],
        description: "Open scriptlet selector popup",
        context:
          "In the Description field, type @ to search and insert a scriptlet at the cursor position",
      },
    ],
  },
  {
    name: "Scriptlet Selector",
    shortcuts: [
      {
        keys: ["\u2191", "\u2193"],
        description: "Navigate through scriptlet list",
        context: "When scriptlet selector popup is open",
      },
      {
        keys: ["Enter"],
        description: "Insert selected scriptlet",
        context: "When scriptlet selector popup is open",
      },
      {
        keys: ["Escape"],
        description: "Close scriptlet selector",
        context: "When scriptlet selector popup is open",
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
    title: "Configuration won't load",
    description: "Loading a JSON or YAML config file fails or shows errors.",
    solution: [
      "Verify the config file is valid JSON or YAML (use a linter)",
      "Check that the config follows the expected schema",
      "Ensure all referenced images/assets exist at the specified paths",
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
    title: "Screenshots not capturing",
    description: "Screen capture returns blank images or fails entirely.",
    solution: [
      "Ensure the target application is visible on screen",
      "Check that the correct monitor is selected",
      "Verify the runner has screen capture permissions",
      "Try running the application as administrator (Windows)",
    ],
  },
  {
    title: "Workflow execution hangs",
    description: "A workflow starts but never completes or times out.",
    solution: [
      "Check the Logs tab for any error messages",
      "Verify the expected UI state is actually visible on screen",
      "Increase timeout values in the workflow configuration",
      "Use the Stop button to halt execution and review logs",
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
    description: "Learn the basics of creating and running automation workflows",
    url: "https://docs.qontinui.io/getting-started",
    icon: Rocket,
  },
  {
    title: "Configuration Reference",
    description: "Complete reference for workflow configuration files",
    url: "https://docs.qontinui.io/configuration",
    icon: FileText,
  },
  {
    title: "API Documentation",
    description: "REST API and MCP integration documentation",
    url: "https://docs.qontinui.io/api",
    icon: Zap,
  },
  {
    title: "Troubleshooting Guide",
    description: "In-depth solutions for common issues",
    url: "https://docs.qontinui.io/troubleshooting",
    icon: Wrench,
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
                <div key={idx} className="p-4 flex items-start gap-4">
                  <div className="flex items-center gap-1.5 flex-shrink-0">
                    {shortcut.keys.map((key, keyIdx) => (
                      <span key={keyIdx}>
                        <kbd className="px-2 py-1 text-sm font-mono bg-muted border border-border rounded shadow-sm">
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
      <div className="bg-blue-500/10 border border-blue-500/20 rounded-lg p-4">
        <div className="flex items-start gap-3">
          <HelpCircle className="w-5 h-5 text-blue-500 flex-shrink-0 mt-0.5" />
          <div>
            <h4 className="font-medium text-blue-500">Tip: Using Scriptlets</h4>
            <p className="text-sm text-muted-foreground mt-1">
              Scriptlets are reusable text snippets that capture learnings from AI debugging
              sessions. Create them in the <strong>Scriptlets</strong> tab, then insert them into
              script descriptions using the dropdown button or by typing{" "}
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
          <CheckCircle2 className="w-5 h-5 text-green-500" />
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
          Qontinui Runner is a desktop application for executing visual automation workflows. It
          uses computer vision and AI to interact with applications, making it possible to automate
          tasks that traditional automation tools cannot handle.
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
              <div className="font-medium">Visual Automation</div>
              <p className="text-sm text-muted-foreground">
                Automate any application using screen capture and image recognition
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
              <div className="font-medium">Workflow Execution</div>
              <p className="text-sm text-muted-foreground">
                Run complex multi-step workflows with state management
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
            icon={ExternalLink}
            label="Connect to a Project"
            description="Link the runner to your Qontinui project for remote management"
          />
          <ChecklistItem
            icon={FolderOpen}
            label="Load a Configuration"
            description="Import a workflow configuration file (JSON or YAML)"
          />
          <ChecklistItem
            icon={Play}
            label="Run Your First Workflow"
            description="Execute a workflow and observe the automation in action"
          />
        </div>
      </div>

      {/* Tip */}
      <div className="bg-blue-500/10 border border-blue-500/20 rounded-lg p-4">
        <div className="flex items-start gap-3">
          <HelpCircle className="w-5 h-5 text-blue-500 flex-shrink-0 mt-0.5" />
          <div>
            <h4 className="font-medium text-blue-500">Need Help?</h4>
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
            href="https://github.com/qontinui/qontinui"
            target="_blank"
            rel="noopener noreferrer"
            className="flex items-center gap-3 text-sm hover:text-primary transition-colors"
          >
            <Github className="w-4 h-4" />
            <span>GitHub Repository</span>
            <ExternalLink className="w-3 h-3 text-muted-foreground" />
          </a>
          <a
            href="https://github.com/qontinui/qontinui/discussions"
            target="_blank"
            rel="noopener noreferrer"
            className="flex items-center gap-3 text-sm hover:text-primary transition-colors"
          >
            <HelpCircle className="w-4 h-4" />
            <span>Community Discussions</span>
            <ExternalLink className="w-3 h-3 text-muted-foreground" />
          </a>
          <a
            href="https://github.com/qontinui/qontinui/releases"
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
                <li key={idx} className="flex items-start gap-2 text-sm text-muted-foreground">
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
          <TroubleshootingItemComponent key={idx} item={item} />
        ))}
      </div>

      {/* Still Need Help */}
      <div className="bg-amber-500/10 border border-amber-500/20 rounded-lg p-4">
        <div className="flex items-start gap-3">
          <Bug className="w-5 h-5 text-amber-500 flex-shrink-0 mt-0.5" />
          <div>
            <h4 className="font-medium text-amber-600 dark:text-amber-400">Still Having Issues?</h4>
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

interface VersionInfo {
  current_version: string;
}

function AboutPage() {
  const [version, setVersion] = useState<string>("0.1.0");
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
            <p className="text-sm text-muted-foreground mt-1">Desktop automation application</p>
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
            <Github className="w-4 h-4" />
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
          <HelpCircle className="w-5 h-5 text-primary flex-shrink-0 mt-0.5" />
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
    const stored = localStorage.getItem(STORAGE_KEY);
    if (
      stored &&
      ["shortcuts", "getting-started", "documentation", "troubleshooting", "about"].includes(stored)
    ) {
      return stored as HelpSubPage;
    }
    return "getting-started";
  });

  // Persist active tab
  useEffect(() => {
    localStorage.setItem(STORAGE_KEY, activeSubPage);
  }, [activeSubPage]);

  const subPages = [
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
          <h3 className="text-lg font-semibold flex items-center gap-2">
            <HelpCircle className="w-5 h-5 text-primary" />
            Help
          </h3>
        </div>

        {subPages.map((page) => {
          const Icon = page.icon;
          return (
            <Tabs.Trigger
              key={page.id}
              value={page.id}
              className={`
                flex items-center gap-3 px-3 py-2.5 rounded-md text-left text-sm font-medium
                transition-colors duration-150 outline-none
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
        <Tabs.Content value="shortcuts" className="outline-none">
          <ShortcutsPage />
        </Tabs.Content>

        <Tabs.Content value="getting-started" className="outline-none">
          <GettingStartedPage />
        </Tabs.Content>

        <Tabs.Content value="documentation" className="outline-none">
          <DocumentationPage />
        </Tabs.Content>

        <Tabs.Content value="troubleshooting" className="outline-none">
          <TroubleshootingPage />
        </Tabs.Content>

        <Tabs.Content value="about" className="outline-none">
          <AboutPage />
        </Tabs.Content>
      </div>
    </Tabs.Root>
  );
}
