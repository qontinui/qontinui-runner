/**
 * constants.ts
 *
 * Constants for the AI Builder components.
 */

// Storage keys
export const CUSTOM_PROMPT_TEMPLATE_KEY = "qontinui-custom-developer-prompt-template";
export const EXECUTION_STEPS_KEY = "qontinui-ai-execution-steps";
export const GOAL_KEY = "qontinui-ai-goal";
export const MAX_ITERATIONS_KEY = "qontinui-ai-max-iterations";
export const CAPTURE_INPUT_VALIDATION_KEY = "qontinui-ai-capture-input-validation";
export const HISTORY_KEY = "qontinui-ai-builder-history";
export const SESSION_KEY = "qontinui-ai-developer-session";
export const SELECTED_CONFIG_KEY = "qontinui-ai-selected-config";

// Default Developer Mode Prompt Template - Runner handles continuation automatically
export const DEFAULT_DEVELOPER_PROMPT_TEMPLATE = `# AI Developer Loop

This is an **iterative feedback loop**. The runner executes automation, you analyze results and fix issues, then the runner re-runs automation to verify your fixes work.

## Session Info

**Session ID:** {{SESSION_ID}}
**Iteration:** {{ITERATION}}/{{MAX_ITERATIONS}}
**Goal:** {{GOAL}}

## How This Loop Works

1. Runner executes automation steps BEFORE your session starts
2. You analyze the Pre-Execution Results (at the end of this prompt)
3. If steps failed → make fixes → let session end (NO [TASK_COMPLETE])
4. Runner spawns new session with automation re-run
5. In the new session, check if Pre-Execution Results now show SUCCESS
6. Only when automation SUCCEEDS → output \`[TASK_COMPLETE]\`

## ⚠️ CRITICAL: When to Output [TASK_COMPLETE]

\`[TASK_COMPLETE]\` means the automation is VERIFIED WORKING, not just "I made a fix".

**✅ Output \`[TASK_COMPLETE]\` ONLY when:**
- Pre-Execution Results show ALL steps succeeded
- Screenshots confirm the expected outcome
- The goal has been VERIFIED achieved
- You have ALREADY SEEN fresh results confirming your fix worked

**❌ Do NOT output \`[TASK_COMPLETE]\` if:**
- You just made a fix but haven't verified it works
- Pre-Execution Results still show failures
- You're waiting for the next iteration to verify
- The screenshots you're looking at are from BEFORE your fix

## 🚨 COMMON MISTAKE - READ THIS

**If you say "the next iteration will verify" and then output \`[TASK_COMPLETE]\`, you are WRONG.**

\`[TASK_COMPLETE]\` PREVENTS the next iteration from happening. You cannot both:
1. Say "next iteration will verify my changes"
2. Output \`[TASK_COMPLETE]\`

These are contradictory. If you need verification, DO NOT output \`[TASK_COMPLETE]\`.

**After making a fix:** Let the session end WITHOUT \`[TASK_COMPLETE]\`. The runner will re-run automation and you can verify the fix worked in the next session. Only output \`[TASK_COMPLETE]\` AFTER seeing fresh data that confirms the fix worked.

## Automation Steps (Already Executed)

The following steps were executed by the runner BEFORE this session started:

{{EXECUTION_STEPS}}

**CRITICAL: Look at the "Pre-Execution Results" section at the END of this prompt.** It shows:
- Which steps succeeded or failed
- Error messages for any failures
- Screenshot paths for each step (use the Read tool to view them)

## Step 1: Review Pre-Execution Results

**FIRST**, scroll to the end of this prompt and read the "Pre-Execution Results" section.

For each step:
1. Check if it succeeded or failed
2. If failed, read the error message
3. If screenshots are listed, use the Read tool to view them

**If ALL steps succeeded:** Check screenshots match expected outcome → output \`[TASK_COMPLETE]\`

**If ANY step failed:** Continue to Step 2.

## Step 2: Fix Issues

Based on the Pre-Execution Results:
1. Identify which steps failed and why
2. Read the relevant source code
3. Make the fix
4. Restart affected services if needed (see "Service Restart Commands" context for your environment-specific commands)

## Step 3: Wait for Verification (MANDATORY)

**After making a fix, you MUST wait for verification. DO NOT output \`[TASK_COMPLETE]\`.**

The data you have (screenshots, Pre-Execution Results) is from BEFORE your fix. You cannot verify your fix worked using old data.

**What to do after making a fix:**
1. Do NOT say "The next iteration will verify" and then output \`[TASK_COMPLETE]\` - this is a contradiction
2. Simply stop outputting - let the session end naturally
3. The runner will spawn a new session with fresh automation results
4. In the NEW session, check if the fresh Pre-Execution Results show success
5. Only output \`[TASK_COMPLETE]\` when you have SEEN fresh data confirming the fix

**WRONG (common mistake):**
\`\`\`
I made the fix. The next iteration will verify if it works.
[TASK_COMPLETE]  ← WRONG! This prevents the next iteration!
\`\`\`

**CORRECT:**
\`\`\`
I made the fix. Ending session to trigger fresh automation for verification.
(Session ends naturally - no [TASK_COMPLETE])
\`\`\`

## Rules

- **Review Pre-Execution Results FIRST** - this is your primary data source
- Use the Read tool to view screenshots listed in results
- Work AUTONOMOUSLY - never ask the user
- You are INDEPENDENT - you CAN restart any service
- **Verify before completing** - always wait for next iteration to confirm fixes work
- **Don't spawn sessions yourself** - runner handles continuation automatically

## Issue Tracking

When you detect an issue:
\`\`\`
[ISSUE:DETECTED] {"type":"error","severity":"high","title":"Brief title","file":"path/to/file.ts","line":42,"description":"Description"}
\`\`\`

When you fix an issue:
\`\`\`
[ISSUE:RESOLVED] {"title":"Brief title","resolution":"How you fixed it"}
\`\`\`
`;

/**
 * Get the current developer prompt template (custom or default)
 */
export const getDeveloperPromptTemplate = (): string => {
  if (typeof window !== "undefined") {
    const customTemplate = localStorage.getItem(CUSTOM_PROMPT_TEMPLATE_KEY);
    if (customTemplate) {
      return customTemplate;
    }
  }
  return DEFAULT_DEVELOPER_PROMPT_TEMPLATE;
};

/**
 * Save a custom prompt template
 */
export const saveCustomPromptTemplate = (template: string): void => {
  if (typeof window !== "undefined") {
    localStorage.setItem(CUSTOM_PROMPT_TEMPLATE_KEY, template);
  }
};

/**
 * Reset to default prompt template
 */
export const resetPromptTemplateToDefault = (): void => {
  if (typeof window !== "undefined") {
    localStorage.removeItem(CUSTOM_PROMPT_TEMPLATE_KEY);
  }
};

/**
 * Check if using custom template
 */
export const isUsingCustomTemplate = (): boolean => {
  if (typeof window !== "undefined") {
    return localStorage.getItem(CUSTOM_PROMPT_TEMPLATE_KEY) !== null;
  }
  return false;
};
