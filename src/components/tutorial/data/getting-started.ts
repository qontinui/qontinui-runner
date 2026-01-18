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
    "Learn the basics of the Qontinui Runner desktop application. This tutorial covers navigation, key features, and how to get started with visual automation.",
  duration: "5 minutes",
  difficulty: "beginner",
  mode: "contextual",
  focusPage: "run",
  category: "Getting Started",
  tags: ["basics", "introduction", "navigation"],
  learningObjectives: [
    "Understand the main sections of the Runner interface",
    "Learn how to navigate using the sidebar",
    "Discover key features for automation",
  ],
  steps: [
    {
      id: "welcome",
      title: "Welcome to Qontinui Runner!",
      content: `Qontinui Runner is your desktop companion for running visual automations.

This powerful tool allows you to:
• Execute automation workflows on your local machine
• Monitor automation progress in real-time
• Integrate with AI for intelligent analysis
• Debug and troubleshoot automation issues

Let's take a quick tour of the interface!`,
      estimatedDuration: 1,
    },
    {
      id: "sidebar-navigation",
      title: "Navigate with the Sidebar",
      content: `The sidebar on the left is your main navigation hub. It's organized into logical groups:

**RUN** - Execute and control your automations
**OBSERVE** - Monitor logs, screenshots, and results
**BUILD** - Configure and manage workflows
**OTHER** - Settings, help, and additional tools

Click any icon to switch between different sections of the app.`,
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
      id: "execute-tab",
      title: "The Execute Tab",
      content: `The Execute tab is where you'll spend most of your time. Here you can:

• **Load configurations** - Import your automation workflow files
• **Select workflows** - Choose which automation to run
• **Choose monitors** - Pick which screen(s) to automate
• **Start/Stop execution** - Control your automations

This is your mission control for running automations!`,
      action: "Look for the Execute tab in the sidebar (play icon)",
      targetElement: {
        selector: "execute-tab",
        highlightType: "border",
        position: "right",
      },
      estimatedDuration: 1,
    },
    {
      id: "logs-section",
      title: "Monitoring with Logs",
      content: `The Logs section gives you real-time visibility into what's happening:

• **General logs** - Overall automation status and events
• **Action logs** - Detailed record of each automation step
• **Image recognition** - Results from visual pattern matching
• **AI output** - Responses from AI analysis

Use logs to understand automation behavior and debug issues.`,
      tips: [
        "Logs are searchable - use the filter to find specific events",
        "Click on log entries to see more details",
        "Logs persist between sessions for debugging",
      ],
      estimatedDuration: 1,
    },
    {
      id: "settings-overview",
      title: "Configure Your Experience",
      content: `The Settings section lets you customize the Runner:

• **AI Settings** - Configure AI providers and models
• **General** - Appearance and behavior options
• **Storage** - Manage screenshots and data
• **Advanced** - Debug options and technical settings

We recommend setting up AI integration to unlock intelligent automation features.`,
      tips: [
        "Start with the AI Settings if you want to use AI analysis",
        "Check Advanced settings for debugging options",
      ],
      estimatedDuration: 1,
    },
    {
      id: "ready-to-go",
      title: "You're Ready!",
      content: `Congratulations! You now know the basics of Qontinui Runner.

**Next Steps:**
1. Load a configuration file to get started
2. Explore the Workflow Execution tutorial for hands-on practice
3. Set up AI integration for intelligent automation

You can access this tutorial and others anytime from the Help section.

Happy automating!`,
      tips: [
        "Check the Help tab for more tutorials",
        "Documentation is available online for advanced features",
      ],
      estimatedDuration: 1,
    },
  ],
};
