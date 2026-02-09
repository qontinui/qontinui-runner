/**
 * Getting Started Tutorial
 *
 * Introduction to the Qontinui Runner for new users.
 */

import type { Tutorial } from "../../../types/tutorial";

export const gettingStartedTutorial: Tutorial = {
  id: "getting-started",
  title: "Welcome to Qontinui Runner",
  description:
    "Get up and running with Qontinui Runner. Configure your AI provider, set up a project, and run your first agentic workflow.",
  duration: "5 minutes",
  difficulty: "beginner",
  mode: "contextual",
  focusPage: "gui-automation",
  category: "Getting Started",
  tags: ["basics", "introduction", "navigation"],
  learningObjectives: [
    "Configure an AI provider to power your workflows",
    "Understand the sidebar navigation and main sections",
    "Build and run your first agentic workflow",
  ],
  steps: [
    {
      id: "welcome",
      title: "Welcome to Qontinui Runner!",
      content: `Qontinui Runner enables autonomous AI-assisted development. AI agents work on your codebase with real-time visual feedback, self-verification, and iterative improvement.

With Qontinui Runner you can:
• Run agentic workflows that automate development tasks
• Watch AI agents work with real-time logs and screenshots
• Verify results automatically with built-in checks
• Iterate until the task is done right

Let's get you set up!`,
      estimatedDuration: 1,
    },
    {
      id: "configure-ai",
      title: "Configure Your AI Provider",
      content: `First, set up an AI provider so the runner can work with AI agents.

Go to **Settings > AI Providers** in the sidebar.

**Recommended:** Select **Claude Code CLI** if you have a Claude Code subscription — it's the easiest option with no per-token cost.

**Alternative:** Select **Claude API** or **Gemini** and enter your API key.

After configuring, click **Test Connection** to verify everything works.`,
      tips: [
        "Claude Code CLI works with your existing subscription",
        "API keys are stored securely in your OS keychain",
      ],
      targetElement: {
        selector: "sidebar",
        highlightType: "spotlight",
        position: "right",
      },
      estimatedDuration: 1,
    },
    {
      id: "sidebar-navigation",
      title: "Navigate with the Sidebar",
      content: `The sidebar on the left is your main navigation hub:

**RUN** — Dashboard and execution controls
**OBSERVE** — Logs, screenshots, and test results
**BUILD** — Workflow builder, test builder, and script tools
**OTHER** — Settings, help, and additional tools

Click any icon to switch between sections.`,
      tips: [
        "The sidebar collapses to icons only to save space",
        "Hover over icons to see tooltips with section names",
      ],
      targetElement: {
        selector: "sidebar",
        highlightType: "spotlight",
        position: "right",
      },
      estimatedDuration: 1,
    },
    {
      id: "build-workflow",
      title: "Build a Workflow",
      content: `Open the **Workflow Builder** from the BUILD section of the sidebar.

A workflow defines what the AI should do. It has four phases:
• **Setup** — One-time preparation steps (run once)
• **Verification** — Check if the task is complete
• **Agentic** — AI works on the task (loops with verification)
• **Completion** — Final steps after success

Click a phase button to add your first step.`,
      action: "Open the Workflow Builder in the sidebar",
      targetElement: {
        selector: "execute-tab",
        highlightType: "border",
        position: "right",
      },
      estimatedDuration: 1,
    },
    {
      id: "run-workflow",
      title: "Run Your Workflow",
      content: `Once your workflow has steps, go to the **Dashboard** and start a new run.

The dashboard shows:
• **Live execution status** — What the AI is doing right now
• **Logs** — Real-time output from each step
• **Verification results** — Whether checks are passing
• **AI conversation** — The full AI interaction

You can stop execution at any time.`,
      tips: [
        "Use the Logs tab to see detailed output from each step",
        "The AI tab shows the full conversation with the AI agent",
      ],
      estimatedDuration: 1,
    },
    {
      id: "ready-to-go",
      title: "You're Ready!",
      content: `You now know the basics of Qontinui Runner.

**Quick recap:**
1. Configure your AI provider in Settings
2. Build a workflow in the Workflow Builder
3. Run it from the Dashboard and watch the AI work

Check the **Help** tab for more tutorials, troubleshooting tips, and documentation links.`,
      tips: [
        "The Help tab has interactive tutorials for each feature",
        "Visit github.com/qontinui/qontinui-runner for documentation",
      ],
      estimatedDuration: 1,
    },
  ],
};
