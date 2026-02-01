/**
 * Workflow Execution Tutorial
 *
 * Step-by-step guide to loading and running automation workflows.
 */

import type { Tutorial } from "../../../types/tutorial";

export const workflowExecutionTutorial: Tutorial = {
  id: "workflow-execution",
  title: "Running Your First Workflow",
  description:
    "Learn how to load a configuration file, select a workflow, and execute your first automation. Perfect for users who have a config file ready to go.",
  duration: "8 minutes",
  difficulty: "beginner",
  mode: "contextual",
  focusPage: "gui-automation",
  category: "Execution",
  tags: ["workflow", "execution", "config", "automation"],
  prerequisites: ["getting-started"],
  learningObjectives: [
    "Load a configuration file into the Runner",
    "Understand workflow selection",
    "Execute an automation workflow",
    "Monitor execution progress",
  ],
  steps: [
    {
      id: "overview",
      title: "What You'll Learn",
      content: `In this tutorial, you'll learn the complete workflow for running an automation:

1. **Load a configuration** - Import your workflow definition file
2. **Select a workflow** - Choose which automation to run
3. **Configure monitors** - Set up screen targeting
4. **Execute** - Start the automation and monitor progress

By the end, you'll be running automations like a pro!`,
      estimatedDuration: 1,
    },
    {
      id: "load-config",
      title: "Loading a Configuration",
      content: `Configuration files define your automations. They contain:

• **States** - Visual elements the automation can recognize
• **Transitions** - How to move between states
• **Workflows** - Sequences of actions to perform

To load a configuration:
1. Click the "Load Config" button
2. Navigate to your .json or .yaml file
3. Select the file and click Open

The Runner will parse the file and show available workflows.`,
      action: "Click the Load Config button to select your configuration file",
      targetElement: {
        selector: "load-config-button",
        highlightType: "spotlight",
        position: "bottom",
      },
      tips: [
        "Configuration files are typically .json or .yaml format",
        "You can also drag and drop config files onto the app",
        "The last loaded config is remembered between sessions",
      ],
      estimatedDuration: 2,
    },
    {
      id: "select-workflow",
      title: "Selecting a Workflow",
      content: `Once a configuration is loaded, you'll see available workflows in a dropdown.

Each workflow represents a specific automation task:
• They have descriptive names (e.g., "Build Queue", "Daily Tasks")
• They may be grouped into categories
• They contain a sequence of actions to execute

Select the workflow you want to run from the dropdown.`,
      action: "Choose a workflow from the dropdown menu",
      targetElement: {
        selector: "workflow-selector",
        highlightType: "border",
        position: "bottom",
      },
      tips: [
        "Workflows are organized by category if defined in the config",
        "The selected workflow persists between sessions",
      ],
      estimatedDuration: 1,
    },
    {
      id: "monitor-selection",
      title: "Choosing Your Monitor",
      content: `The Runner needs to know which screen to automate.

**Why this matters:**
• Screenshots are captured from the selected monitor
• Mouse/keyboard actions target that screen
• Multi-monitor setups require explicit selection

Select the monitor where your target application is displayed.`,
      action: "Select the monitor to use for automation",
      targetElement: {
        selector: "monitor-selector",
        highlightType: "border",
        position: "bottom",
      },
      tips: [
        "Monitor numbers correspond to your OS display settings",
        "Use 'Detect Monitors' to refresh the list",
        "You can select multiple monitors for advanced scenarios",
      ],
      estimatedDuration: 1,
    },
    {
      id: "start-execution",
      title: "Starting Execution",
      content: `With everything configured, you're ready to run!

Click the **Start** button to begin execution. The automation will:
1. Initialize the Python executor
2. Begin the selected workflow
3. Execute actions step by step
4. Report progress in the logs

You can stop execution at any time with the Stop button.`,
      action: "Click Start to begin the automation",
      targetElement: {
        selector: "start-execution-button",
        highlightType: "pulse",
        position: "bottom",
      },
      shortcuts: ["Ctrl+Enter to start", "Escape to stop"],
      estimatedDuration: 1,
    },
    {
      id: "monitor-progress",
      title: "Monitoring Progress",
      content: `While running, you can monitor progress in several ways:

• **Status Banner** - Shows current execution state
• **Action Logs** - Real-time log of actions being performed
• **State Visualization** - Visual representation of state machine
• **Screenshots** - Captured screens during execution

Check the Logs section to see detailed execution information.`,
      tips: [
        "The action log shows each step as it executes",
        "Failed actions are highlighted in red",
        "Screenshots are saved automatically for debugging",
      ],
      estimatedDuration: 1,
    },
    {
      id: "completion",
      title: "Great Job!",
      content: `You've learned how to run a workflow from start to finish!

**Summary:**
✓ Load configuration files
✓ Select and configure workflows
✓ Choose target monitors
✓ Execute and monitor automations

**Next Steps:**
• Try the AI Analysis tutorial to add intelligence
• Explore the Logs section for debugging
• Check Settings for advanced options

You're now ready to automate!`,
      estimatedDuration: 1,
    },
  ],
};
