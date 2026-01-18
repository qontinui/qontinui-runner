/**
 * AI Analysis Tutorial
 *
 * Guide to using AI features for intelligent automation analysis.
 */

import type { Tutorial } from "../../../types/tutorial";

export const aiAnalysisTutorial: Tutorial = {
  id: "ai-analysis",
  title: "AI-Powered Automation",
  description:
    "Learn how to leverage AI capabilities for intelligent automation analysis, debugging, and optimization. Requires API access to Claude or compatible AI providers.",
  duration: "10 minutes",
  difficulty: "intermediate",
  mode: "contextual",
  focusPage: "ai",
  category: "AI Features",
  tags: ["ai", "claude", "analysis", "debugging", "optimization"],
  prerequisites: ["getting-started", "workflow-execution"],
  learningObjectives: [
    "Configure AI provider settings",
    "Understand AI execution modes",
    "Use AI for automation analysis",
    "Interpret AI findings and recommendations",
  ],
  steps: [
    {
      id: "intro",
      title: "AI-Powered Automation",
      content: `Qontinui Runner integrates with AI to enhance your automations:

**What AI Can Do:**
• Analyze automation failures and suggest fixes
• Understand screenshots and identify UI elements
• Generate insights from execution logs
• Help debug complex state machine issues

This tutorial will show you how to set up and use these features.`,
      estimatedDuration: 1,
    },
    {
      id: "ai-settings",
      title: "Configuring AI Settings",
      content: `First, you'll need to configure your AI provider.

Go to **Settings → AI Settings** to configure:

• **Provider** - Currently supports Claude (Anthropic)
• **API Key** - Your authentication credentials
• **Model** - Which AI model to use (e.g., claude-sonnet)
• **Execution Mode** - How AI is triggered

Your API key is stored locally and never shared.`,
      action: "Navigate to Settings → AI Settings",
      targetElement: {
        selector: "ai-settings-panel",
        highlightType: "spotlight",
        position: "left",
      },
      tips: [
        "Get an API key from anthropic.com",
        "Start with claude-sonnet for a balance of speed and capability",
        "Your key is stored securely in local storage",
      ],
      estimatedDuration: 2,
    },
    {
      id: "execution-modes",
      title: "AI Execution Modes",
      content: `The Runner supports different ways to trigger AI analysis:

**Manual Mode**
• You explicitly request AI analysis
• Best for on-demand debugging
• Full control over when AI runs

**Automatic Mode**
• AI analyzes failures automatically
• Provides immediate feedback
• Great for development workflows

**Orchestrated Mode**
• AI manages the entire debugging loop
• Iteratively fixes issues until resolved
• Most autonomous option

Choose based on your workflow and preferences.`,
      tips: [
        "Start with Manual mode to understand AI capabilities",
        "Switch to Automatic for faster feedback during development",
      ],
      estimatedDuration: 2,
    },
    {
      id: "triggering-analysis",
      title: "Triggering AI Analysis",
      content: `To manually trigger AI analysis:

1. Run your automation workflow
2. If issues occur (or anytime), click "Trigger AI Analysis"
3. AI will examine logs, screenshots, and configuration
4. Results appear in the AI Output section

The AI receives context about:
• Current workflow and state
• Recent execution logs
• Screenshots and visual context
• Configuration details`,
      action: "Look for the AI Analysis button in the execute section",
      estimatedDuration: 1,
    },
    {
      id: "understanding-findings",
      title: "Understanding AI Findings",
      content: `AI analysis produces structured findings:

**Finding Types:**
• **Code Bug** - Issues in your configuration or workflow
• **Observation** - Notable patterns or behaviors
• **Hypothesis** - Potential explanations for issues
• **Recommendation** - Suggested improvements

Each finding includes:
• Severity level (critical, high, medium, low)
• Detailed description
• Affected components
• Suggested resolution

Review findings in the Reports section.`,
      tips: [
        "Critical findings should be addressed first",
        "Code bugs often include specific fixes",
        "Recommendations may improve reliability even without failures",
      ],
      estimatedDuration: 2,
    },
    {
      id: "ai-output",
      title: "AI Output Logs",
      content: `The AI Output section shows the full AI conversation:

• See exactly what context AI received
• Read AI's reasoning and analysis
• Track iterations in orchestrated mode
• Export for documentation

This transparency helps you:
• Understand AI's decision process
• Verify recommendations make sense
• Learn patterns for future debugging`,
      estimatedDuration: 1,
    },
    {
      id: "best-practices",
      title: "AI Best Practices",
      content: `Get the most out of AI features:

**Do:**
• Provide clear workflow names and descriptions
• Let executions run to completion for full context
• Review AI suggestions before implementing
• Use AI to learn about your automation's behavior

**Don't:**
• Share sensitive data in configurations
• Rely solely on AI without understanding
• Ignore security recommendations

AI is a powerful tool that complements your expertise.`,
      tips: [
        "AI analysis works best with descriptive configurations",
        "Run multiple iterations for complex issues",
        "Save useful AI insights for future reference",
      ],
      estimatedDuration: 1,
    },
    {
      id: "summary",
      title: "AI Features Unlocked!",
      content: `You're now ready to use AI-powered automation features!

**What You Learned:**
✓ Configure AI provider settings
✓ Choose appropriate execution modes
✓ Trigger and interpret AI analysis
✓ Apply AI recommendations

**Advanced Tips:**
• Combine AI analysis with manual debugging
• Use orchestrated mode for hands-off fixing
• Check AI output logs for learning opportunities

Your automations just got smarter!`,
      estimatedDuration: 1,
    },
  ],
};
