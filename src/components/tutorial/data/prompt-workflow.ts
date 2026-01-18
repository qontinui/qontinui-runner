/**
 * AI Prompt Workflow Tutorial (Contextual/Interactive)
 *
 * Interactive guide to creating workflows with AI prompts for verification and agentic steps.
 * Uses contextual mode with spotlights and positioned tooltips.
 */

import type { Tutorial } from "../../../types/tutorial";

export const promptWorkflowTutorial: Tutorial = {
  id: "prompt-workflow",
  title: "Build an AI Prompt Workflow",
  description:
    "Create a powerful AI-driven workflow using prompt steps for both verification and agentic execution. Learn the core pattern that makes Qontinui automation intelligent.",
  duration: "12 minutes",
  difficulty: "intermediate",
  mode: "contextual",
  focusPage: "unified-workflow-builder",
  category: "Workflow Building",
  tags: ["workflow", "ai", "prompts", "verification", "agentic", "automation", "featured"],
  prerequisites: ["getting-started"],
  learningObjectives: [
    "Understand the verification <-> agentic loop pattern",
    "Create verification steps using AI prompts",
    "Create agentic steps using AI prompts",
    "Configure prompt settings and providers",
    "Build a complete AI-driven workflow",
  ],
  steps: [
    {
      id: "intro",
      title: "AI Prompt Workflows",
      content: `Welcome! In this tutorial, you'll learn to create workflows where AI handles both verification (checking goals) and agentic execution (taking action).

This creates an intelligent loop where AI verifies progress and takes corrective action until success.`,
      estimatedDuration: 1,
    },
    {
      id: "understand-phases",
      title: "Understanding Workflow Phases",
      content: `Workflows have four phases:

**Setup** - Prepare the environment (runs once)
**Verification** - Check success criteria (loops)
**Agentic** - Take action to achieve goals (loops)
**Completion** - Final actions (runs once)

The Verification <-> Agentic loop is where the magic happens!`,
      estimatedDuration: 1,
    },
    {
      id: "navigate-builder",
      title: "Open the Workflow Builder",
      content: `This is the Workflow Builder where you create AI-driven workflows.

The left panel shows your saved workflows. The right panel is the workflow editor with phases.`,
      action: "Navigate to Workflows in the sidebar if not already there",
      targetElement: {
        selector: "workflow-builder-nav",
        highlightType: "spotlight",
        position: "left",
        allowInteraction: true,
      },
      estimatedDuration: 1,
    },
    {
      id: "create-workflow",
      title: "Create a New Workflow",
      content: `Click the + button to create a new workflow.

Give it a descriptive name like "Code Review Assistant" or "Bug Fix Automation".`,
      action: "Click + to create a new workflow",
      targetElement: {
        selector: "new-workflow-button",
        highlightType: "pulse",
        position: "right",
        allowInteraction: true,
      },
      wait: {
        type: "dom-event",
        event: "click",
        selector: "[data-tutorial-id='new-workflow-button']",
        timeout: 30000,
        onTimeout: "show-hint",
        hint: "Click the + button in the top-right of the Workflows panel",
      },
      estimatedDuration: 1,
    },
    {
      id: "verification-phase",
      title: "The Verification Phase",
      content: `This is the Verification Phase (green). It checks if your goals are achieved.

Add AI prompts here that evaluate success criteria. If verification fails, the agentic phase runs.`,
      action: "Look at the Verification Phase section",
      targetElement: {
        selector: "verification-phase",
        highlightType: "spotlight",
        position: "left",
        allowInteraction: true,
      },
      estimatedDuration: 1,
    },
    {
      id: "agentic-phase",
      title: "The Agentic Phase",
      content: `This is the Agentic Phase (amber/orange). It takes action when verification fails.

Add AI prompts here that perform work to achieve your goals. After execution, verification runs again.`,
      action: "Look at the Agentic Phase section",
      targetElement: {
        selector: "agentic-phase",
        highlightType: "spotlight",
        position: "left",
        allowInteraction: true,
      },
      estimatedDuration: 1,
    },
    {
      id: "add-verification-prompt",
      title: "Add a Verification Prompt",
      content: `To add an AI prompt to verification:
1. Click "+ Add" in the Verification Phase
2. Select "AI Prompt" from the menu
3. Configure your prompt in the panel on the right

The AI will check success criteria and return whether they're met.`,
      action: "Add an AI Prompt step to the Verification Phase",
      targetElement: {
        selector: "verification-phase",
        highlightType: "border",
        position: "left",
        allowInteraction: true,
      },
      wait: {
        type: "tauri-event",
        tauriEvent: "workflow-step-added",
        filter: (payload: unknown) => {
          const data = payload as { phase?: string };
          return data?.phase === "verification";
        },
        timeout: 60000,
        onTimeout: "allow-skip",
        hint: "Click the '+ Add' button in the Verification Phase, then select 'AI Prompt'",
      },
      tips: [
        "Take your time to configure the prompt",
        "The tutorial will advance when you add the step",
      ],
      estimatedDuration: 2,
    },
    {
      id: "add-agentic-prompt",
      title: "Add an Agentic Prompt",
      content: `Similarly, add an AI prompt to the Agentic Phase:
1. Click "+ Add" in the Agentic Phase
2. Select "AI Prompt"
3. Configure your prompt in the panel on the right

This prompt tells the AI what action to take when verification fails.`,
      action: "Add an AI Prompt step to the Agentic Phase",
      targetElement: {
        selector: "agentic-phase",
        highlightType: "border",
        position: "left",
        allowInteraction: true,
      },
      wait: {
        type: "tauri-event",
        tauriEvent: "workflow-step-added",
        filter: (payload: unknown) => {
          const data = payload as { phase?: string };
          return data?.phase === "agentic";
        },
        timeout: 60000,
        onTimeout: "allow-skip",
        hint: "Click the '+ Add' button in the Agentic Phase, then select 'AI Prompt'",
      },
      tips: [
        "Take your time to configure the prompt",
        "The tutorial will advance when you add the step",
      ],
      estimatedDuration: 2,
    },
    {
      id: "configure-settings",
      title: "Configure Workflow Settings",
      content: `Click Settings to configure:

**Max Iterations** - How many loops before stopping (default: 10)
**Provider** - Which AI to use (Claude CLI, Gemini)
**Model** - AI model for prompts`,
      action: "Click Settings to view options",
      targetElement: {
        selector: "workflow-settings",
        highlightType: "pulse",
        position: "bottom",
        allowInteraction: true,
      },
      estimatedDuration: 1,
    },
    {
      id: "save-workflow",
      title: "Save Your Workflow",
      content: `Click Save to store your workflow.

Once saved, you can run it and watch the verification-agentic loop in action!`,
      action: "Click Save to store the workflow",
      targetElement: {
        selector: "save-workflow-button",
        highlightType: "pulse",
        position: "bottom",
        allowInteraction: true,
      },
      wait: {
        type: "dom-event",
        event: "click",
        selector: "[data-tutorial-id='save-workflow-button']",
        timeout: 30000,
        onTimeout: "allow-skip",
        hint: "Click the Save button to store your workflow",
      },
      estimatedDuration: 1,
    },
    {
      id: "summary",
      title: "Workflow Complete!",
      content: `You've learned to build AI prompt workflows!

**Key Concepts:**
- Verification prompts check success criteria
- Agentic prompts take corrective action
- The loop continues until success or max iterations

You're now ready to create intelligent, self-correcting automations!`,
      estimatedDuration: 1,
    },
  ],
};
