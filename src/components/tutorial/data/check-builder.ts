/**
 * Check Builder Tutorial (Contextual/Interactive)
 *
 * Interactive guide to creating and managing code quality checks.
 * Uses contextual mode with spotlights and positioned tooltips.
 */

import type { Tutorial } from "../../../types/tutorial";

export const checkBuilderTutorial: Tutorial = {
  id: "check-builder",
  title: "Master Code Quality Checks",
  description:
    "Learn to create and run code quality checks for linting, formatting, and type checking. Set up auto-fix to automatically correct issues.",
  duration: "8 minutes",
  difficulty: "beginner",
  mode: "contextual",
  focusPage: "check-builder",
  category: "Build Tools",
  tags: ["checks", "linting", "formatting", "typecheck", "code-quality", "auto-fix"],
  learningObjectives: [
    "Understand the different types of code checks",
    "Create a new check using built-in tools",
    "Configure auto-fix for automatic corrections",
    "Run checks and interpret results",
    "Add checks to workflows",
  ],
  steps: [
    {
      id: "intro",
      title: "Code Quality Checks",
      content: `Welcome! Code quality checks help you maintain clean, consistent code by running tools like linters, formatters, and type checkers.

**What you'll learn:**
- Create checks for different languages (Python, JavaScript, Rust)
- Use auto-fix to automatically correct issues
- Add checks to your automation workflows`,
      estimatedDuration: 1,
    },
    {
      id: "check-types",
      title: "Types of Checks",
      content: `There are four types of code quality checks:

**Lint** - Find bugs and code smells (ruff, eslint, clippy)
**Format** - Enforce consistent code style (black, prettier, rustfmt)
**Type Check** - Verify type correctness (mypy, tsc)
**Custom** - Run any command-line tool

Formatters support **auto-fix** to automatically correct issues!`,
      estimatedDuration: 1,
      tips: ["Formatters default to auto-fix mode", "Linters can also auto-fix many issues"],
    },
    {
      id: "navigate-builder",
      title: "Open the Check Builder",
      content: `Navigate to the Check Builder in the sidebar.

Go to **BUILD** > **Builders** > **Check Builder**

The Check Builder has three panels:
- Left: Your saved checks
- Center: Check configuration
- Right: Properties and settings`,
      action: "Navigate to Check Builder in the sidebar",
      targetElement: {
        selector: "check-builder-nav",
        highlightType: "spotlight",
        position: "right",
        allowInteraction: true,
      },
      estimatedDuration: 1,
    },
    {
      id: "library-panel",
      title: "The Check Library",
      content: `This is the Check Library panel. It shows all your saved checks organized by type.

You can:
- **Search** for checks by name
- **Filter** by check type (lint, format, etc.)
- **Click** a check to edit it`,
      action: "Look at the left panel showing your checks",
      targetElement: {
        selector: "check-library-panel",
        highlightType: "spotlight",
        position: "right",
        allowInteraction: true,
      },
      estimatedDuration: 1,
    },
    {
      id: "create-check",
      title: "Create a New Check",
      content: `Click the **+** button to create a new check.

You'll see a menu organized by check type:
- **Lint**: ruff, eslint, clippy
- **Format**: black, prettier, rustfmt
- **Type Check**: mypy, tsc

Select a tool to create a check with sensible defaults.`,
      action: "Click + to create a new check",
      targetElement: {
        selector: "new-check-button",
        highlightType: "pulse",
        position: "right",
        allowInteraction: true,
      },
      estimatedDuration: 1,
    },
    {
      id: "configure-command",
      title: "Configure the Command",
      content: `The center panel shows the check configuration.

**Command** - The command to run (pre-filled with defaults)
**Working Directory** - Where to run the command
**Config File** - Path to tool config (e.g., pyproject.toml)

You can customize the command or use the default.`,
      action: "Review the command configuration",
      targetElement: {
        selector: "check-command-input",
        highlightType: "border",
        position: "bottom",
        allowInteraction: true,
      },
      estimatedDuration: 1,
    },
    {
      id: "auto-fix",
      title: "Enable Auto-Fix",
      content: `**Auto-Fix** is a powerful feature that automatically corrects issues.

When enabled, tools like formatters and linters will fix problems instead of just reporting them.

The toggle appears for tools that support auto-fix. **Formatters default to auto-fix ON.**`,
      action: "Look for the Auto-Fix toggle",
      targetElement: {
        selector: "check-autofix-toggle",
        highlightType: "spotlight",
        position: "left",
        allowInteraction: true,
      },
      tips: [
        "Auto-fix is safe - tools only fix what they're confident about",
        "Check results show 'Fixed' status when issues were corrected",
      ],
      estimatedDuration: 1,
    },
    {
      id: "properties-panel",
      title: "Check Properties",
      content: `The right panel shows check properties:

**Name** - Descriptive name for the check
**Description** - What the check verifies
**Timeout** - Maximum execution time
**Critical** - Fail workflow if this check fails
**Tags** - For organizing checks`,
      action: "Review the properties panel",
      targetElement: {
        selector: "check-properties-panel",
        highlightType: "spotlight",
        position: "left",
        allowInteraction: true,
      },
      estimatedDuration: 1,
    },
    {
      id: "run-check",
      title: "Run the Check",
      content: `Click **Run Check** to execute the check immediately.

The results will show:
- **Status**: Passed, Failed, Fixed, or Error
- **Issues Found**: Number of problems detected
- **Issues Fixed**: Number of problems auto-fixed
- **Output**: Full tool output`,
      action: "Click Run Check to test the check",
      targetElement: {
        selector: "run-check-button",
        highlightType: "pulse",
        position: "bottom",
        allowInteraction: true,
      },
      estimatedDuration: 1,
    },
    {
      id: "understand-results",
      title: "Understanding Results",
      content: `Check results have several statuses:

- **Passed** - No issues found
- **Failed** - Issues found but not fixed
- **Fixed** - Issues found and auto-fixed
- **Error** - Tool failed to run
- **Timeout** - Exceeded time limit

The output shows exactly what the tool reported.`,
      tips: [
        "Green = Passed or Fixed (good!)",
        "Red = Failed or Error (needs attention)",
        "Yellow = Timeout (increase timeout in properties)",
      ],
      estimatedDuration: 1,
    },
    {
      id: "save-check",
      title: "Save Your Check",
      content: `Click **Save** to store your check configuration.

Saved checks can be:
- Run anytime from the Check Builder
- Added to workflows as verification steps
- Detected automatically for new projects`,
      action: "Click Save to store the check",
      targetElement: {
        selector: "save-check-button",
        highlightType: "pulse",
        position: "bottom",
        allowInteraction: true,
      },
      estimatedDuration: 1,
    },
    {
      id: "workflow-integration",
      title: "Add to Workflows",
      content: `Checks integrate seamlessly with workflows.

In the Workflow Builder:
1. Go to the **Verification** phase
2. Click **+ Add Step**
3. Select **Checks** and choose a check type

Checks run as part of your automation and can block workflows if marked as **Critical**.`,
      tips: [
        "Use checks in verification to ensure code quality before AI actions",
        "Auto-fix formatters can clean up AI-generated code",
      ],
      estimatedDuration: 1,
    },
    {
      id: "ai-detection",
      title: "AI Project Detection",
      content: `The Check Builder can automatically detect your project type and suggest relevant checks.

Click **Detect Project & Suggest Checks** to:
- Scan for config files (package.json, pyproject.toml, Cargo.toml)
- Identify installed tools
- Create checks with project-specific settings`,
      action: "Try the AI detection feature",
      targetElement: {
        selector: "detect-project-button",
        highlightType: "pulse",
        position: "top",
        allowInteraction: true,
      },
      estimatedDuration: 1,
    },
    {
      id: "summary",
      title: "You're Ready!",
      content: `Congratulations! You now know how to use Code Quality Checks.

**Key Takeaways:**
- Create checks for lint, format, and type checking
- Enable auto-fix to automatically correct issues
- Add checks to workflows for automated code quality
- Use AI detection to set up checks for your projects

Maintain clean, consistent code with automated checks!`,
      estimatedDuration: 1,
    },
  ],
};
