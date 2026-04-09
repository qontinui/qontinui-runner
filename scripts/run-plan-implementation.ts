#!/usr/bin/env npx tsx
/**
 * run-plan-implementation.ts
 *
 * Standalone script that parses a markdown plan and executes it as a
 * plan implementation workflow on the runner. Does not depend on the
 * monorepo type system — uses inline parsing and workflow construction.
 *
 * Usage:
 *   npx tsx scripts/run-plan-implementation.ts <plan-file.md> [--port 9876] [--dry-run] [--follow]
 *   echo "plan text" | npx tsx scripts/run-plan-implementation.ts - [--port 9876]
 *
 * Options:
 *   --port N     Target runner port (default: 9876)
 *   --dry-run    Output workflow JSON without executing
 *   --follow     After submitting, poll and stream stage progress until completion
 */

import { readFileSync } from "fs";
import { randomUUID } from "crypto";

// ─── Args ───────────────────────────────────────────────────────────────────

const args = process.argv.slice(2);
const planFile = args.find((a) => !a.startsWith("--"));
const portFlag = args.indexOf("--port");
const port = portFlag >= 0 ? parseInt(args[portFlag + 1], 10) : 9876;
const dryRun = args.includes("--dry-run");
const follow = args.includes("--follow");

if (!planFile) {
  console.error("Usage: npx tsx scripts/run-plan-implementation.ts <plan-file.md> [--port 9876] [--dry-run] [--follow]");
  process.exit(1);
}

let planContent: string;
if (planFile === "-") {
  planContent = readFileSync(0, "utf-8"); // stdin
} else {
  planContent = readFileSync(planFile, "utf-8");
}

// ─── Plan parser ────────────────────────────────────────────────────────────
// Extracts phases and tasks from markdown plans.
//
// Strategy:
//   1. Numbered headings (### 1. Name, ## Phase 1: Name) → phases
//   2. Under a phase: numbered items and bullet items → tasks
//   3. Numbered items at root level (not under a heading phase) → phases
//   4. Skip non-actionable sections (Context, Goal, Reference, Validation, etc.)

interface PlanTask {
  id: string;
  name: string;
  description: string;
}

interface PlanPhase {
  id: string;
  name: string;
  description: string;
  tasks: PlanTask[];
}

const SKIP_SECTIONS = /^\s*#{1,3}\s+(goal|context|background|reference|validation|appendix|notes|prerequisites?|requirements?|current|what is|how\s)/i;

function parsePlan(text: string): PlanPhase[] {
  const lines = text.split("\n");
  const phases: PlanPhase[] = [];
  let currentPhase: PlanPhase | null = null;
  let inSkippedSection = false;
  let inHeadingPhase = false; // true when the current phase came from a heading

  for (const line of lines) {
    const trimmed = line.trim();

    // Skip empty lines (don't reset anything)
    if (trimmed === "" || trimmed === "---") continue;

    // Detect section headings that should be skipped
    if (SKIP_SECTIONS.test(trimmed)) {
      inSkippedSection = true;
      continue;
    }

    // Any heading resets the skip flag
    if (/^#{1,3}\s+/.test(trimmed)) {
      inSkippedSection = false;
    }

    if (inSkippedSection) continue;

    // ── Numbered headings → phases ──────────────────────────────────────
    // ### 1. Backend: Workflow Event Ingestion API
    // ## Phase 1: Name
    // ## Part 2 — Name
    const numberedHeadingMatch = trimmed.match(
      /^#{1,3}\s+(?:(?:Phase|Part|Step)\s+)?(\d+)[.:)\-—]\s*(.+)/i,
    );
    if (numberedHeadingMatch) {
      if (currentPhase) phases.push(currentPhase);
      const name = numberedHeadingMatch[2].trim();
      currentPhase = {
        id: `phase-${phases.length}`,
        name,
        description: name,
        tasks: [],
      };
      inHeadingPhase = true;
      continue;
    }

    // Non-numbered headings that aren't skipped → could be a section like
    // "## What to Build" or "## Implementation Order". These are structural
    // headings, not phases. Just reset the current phase context so
    // numbered items under them get treated correctly.
    if (/^#{1,2}\s+/.test(trimmed)) {
      // Check if this looks like a meta section (Implementation Order, etc.)
      if (/implementation\s+order|summary|overview/i.test(trimmed)) {
        // Numbered items under this are a summary list, not phases to implement.
        // Only create phases from them if we haven't found heading-based phases yet.
        if (phases.length > 0 || currentPhase) {
          inSkippedSection = true;
        }
      }
      // Don't create a phase from a non-numbered heading
      continue;
    }

    // ── Numbered items ──────────────────────────────────────────────────
    const numberedMatch = trimmed.match(/^(\d+)[.)]\s+(.+)/);
    if (numberedMatch) {
      const itemText = numberedMatch[2].replace(/\*{1,2}/g, "").trim();

      if (inHeadingPhase && currentPhase) {
        // We're inside a heading-based phase → numbered items are tasks
        currentPhase.tasks.push({
          id: `task-${currentPhase.tasks.length}`,
          name: itemText.split(/[—–]/)[0].trim(),
          description: itemText,
        });
      } else {
        // No heading phase active → numbered items at root are phases
        if (currentPhase) phases.push(currentPhase);
        currentPhase = {
          id: `phase-${phases.length}`,
          name: itemText,
          description: itemText,
          tasks: [],
        };
        inHeadingPhase = false;
      }
      continue;
    }

    // ── Bullet items → tasks ────────────────────────────────────────────
    const bulletMatch = trimmed.match(/^[-*]\s+(?:\[.\]\s+)?(.+)/);
    if (bulletMatch && currentPhase) {
      const taskText = bulletMatch[1].replace(/\*{1,2}/g, "").trim();
      currentPhase.tasks.push({
        id: `task-${currentPhase.tasks.length}`,
        name: taskText.split(/[—–:]/)[0].trim(),
        description: taskText,
      });
      continue;
    }

    // ── Continuation text → append to phase description ─────────────────
    if (currentPhase && trimmed.length > 0 && !trimmed.startsWith("#") && !trimmed.startsWith("```")) {
      if (currentPhase.description === currentPhase.name) {
        currentPhase.description = trimmed;
      } else if (currentPhase.description.length < 500) {
        currentPhase.description += " " + trimmed;
      }
    }
  }
  if (currentPhase) phases.push(currentPhase);

  // If phases have no tasks, synthesize one task per phase
  for (const phase of phases) {
    if (phase.tasks.length === 0) {
      phase.tasks.push({
        id: "task-0",
        name: phase.name,
        description: phase.description,
      });
    }
  }

  return phases;
}

// ─── Workflow builder ───────────────────────────────────────────────────────

function buildTaskList(phase: PlanPhase): string {
  return phase.tasks.map((t, i) => `${i + 1}. **${t.name}**: ${t.description}`).join("\n");
}

function makePromptStep(phase: string, name: string, content: string) {
  return { id: randomUUID(), type: "prompt", phase, name, content };
}

function buildImplementPrompt(phase: PlanPhase, idx: number): string {
  return `## Phase ${idx + 1}: ${phase.name}

${phase.description}

### Tasks to implement:
${buildTaskList(phase)}

### Instructions

Implement ALL tasks listed above completely. Requirements:
- No stubs, no partial implementations, no TODOs
- Use subagents (Agent tool) for independent work across files/repos
- Fix any issues you encounter during implementation
- After implementing, run the verification checks to confirm your work compiles and passes

Some verification steps may have failed. Analyze the failures and implement the necessary changes to make all checks pass.`;
}

function buildReviewPrompt(phase: PlanPhase, idx: number): string {
  return `## Review: Phase ${idx + 1} — ${phase.name}

A previous session just implemented this phase. Your job is to review the implementation for completeness, correctness, and bugs, then fix everything you find.

### What was planned:
${buildTaskList(phase)}

### How to review

1. **Understand what changed** — Run: \`git diff HEAD~5 --stat\` and \`git log --oneline -10\`
2. **Audit completeness** — For each planned task, verify code exists, is wired up, no TODOs left behind
3. **Check for bugs** — Run type checkers and linters, review for logical errors
4. **Fix everything** — Do not just report issues. Fix them immediately.

Some verification steps may have failed. Fix the underlying issues to make all checks pass.`;
}

function buildNextStepsPrompt(phase: PlanPhase, idx: number): string {
  return `## Next Steps: Phase ${idx + 1} — ${phase.name}

Previous sessions implemented and reviewed this phase. Find and fix remaining wiring, polish, and integration issues.

### What was planned:
${buildTaskList(phase)}

### How to analyze

Run \`git diff HEAD~10 --stat\` and \`git log --oneline -15\` to understand recent changes.

Look for: missing wiring, unhandled fields, polish, integration gaps, follow-up features.

For each issue found: implement the fix, verify it compiles, move on.

Some verification steps may have failed. Fix the underlying issues to make all checks pass.`;
}

function buildWorkflow(phases: PlanPhase[]) {
  const stages: any[] = [];
  const now = new Date().toISOString();

  for (let i = 0; i < phases.length; i++) {
    const phase = phases[i];

    stages.push({
      id: `${phase.id}-implement`,
      name: `Phase ${i + 1}: Implement — ${phase.name}`,
      description: `Implement all tasks for phase: ${phase.name}`,
      setup_steps: [],
      verification_steps: [],
      agentic_steps: [makePromptStep("agentic", `Implement: ${phase.name}`, buildImplementPrompt(phase, i))],
      completion_steps: [],
      max_iterations: 15,
    });

    stages.push({
      id: `${phase.id}-review`,
      name: `Phase ${i + 1}: Review — ${phase.name}`,
      description: `Review implementation for phase: ${phase.name}`,
      setup_steps: [],
      verification_steps: [],
      agentic_steps: [makePromptStep("agentic", `Review: ${phase.name}`, buildReviewPrompt(phase, i))],
      completion_steps: [],
      max_iterations: 6,
    });

    stages.push({
      id: `${phase.id}-next-steps`,
      name: `Phase ${i + 1}: Next Steps — ${phase.name}`,
      description: `Find and fix remaining issues for phase: ${phase.name}`,
      setup_steps: [],
      verification_steps: [],
      agentic_steps: [makePromptStep("agentic", `Next Steps: ${phase.name}`, buildNextStepsPrompt(phase, i))],
      completion_steps: [],
      max_iterations: 6,
    });
  }

  stages.push({
    id: "commit",
    name: "Commit Changes",
    description: "Commit all implementation changes",
    setup_steps: [],
    verification_steps: [],
    agentic_steps: [],
    completion_steps: [
      makePromptStep("completion", "Commit Changes",
        "## Commit Changes\n\nAll implementation phases are complete. Run `git status` and `git diff --stat`, stage relevant files (exclude .env, credentials), and commit with `feat: <summary>`. Do NOT include AI attribution."),
    ],
    max_iterations: 1,
  });

  const totalTasks = phases.reduce((s, p) => s + p.tasks.length, 0);

  return {
    id: randomUUID(),
    name: `Plan Implementation (${phases.length} phases, ${stages.length} stages)`,
    description: `Auto-generated plan implementation. ${phases.length} phases, ${totalTasks} tasks. Each phase runs implement → review → next-steps.`,
    setup_steps: [],
    verification_steps: [],
    agentic_steps: [],
    completion_steps: [],
    max_iterations: 15,
    stages,
    category: "plan-implementation",
    tags: ["plan", "auto-generated", "multi-stage", "implementation"],
    created_at: now,
    modified_at: now,
  };
}

// ─── Follow mode: poll workflow progress ────────────────────────────────────

interface TaskRunState {
  status: string;
  workflow_state?: string;
  plan_phase_name?: string;
  plan_phase_index?: number;
  plan_total_phases?: number;
  iterations_run?: number;
  verification_passed?: boolean;
}

async function followProgress(taskRunId: string): Promise<void> {
  const baseUrl = `http://localhost:${port}`;
  let lastState = "";
  let lastPhase = "";

  console.error(`\nFollowing workflow progress (task_run_id: ${taskRunId})...\n`);

  while (true) {
    try {
      const res = await fetch(`${baseUrl}/task-runs/${taskRunId}/workflow-state`);
      if (!res.ok) {
        // Task run might not be created yet
        await sleep(3000);
        continue;
      }

      const data = await res.json();
      const state: TaskRunState = data?.data ?? data;

      const status = state.status ?? "unknown";
      const wfState = state.workflow_state ?? "";
      const phaseName = state.plan_phase_name ?? "";
      const phaseIdx = state.plan_phase_index;
      const totalPhases = state.plan_total_phases;
      const iterations = state.iterations_run ?? 0;
      const passed = state.verification_passed;

      // Build status line
      const phaseInfo = phaseName
        ? ` [Phase ${(phaseIdx ?? 0) + 1}/${totalPhases ?? "?"}: ${phaseName}]`
        : "";
      const iterInfo = iterations > 0 ? ` (iteration ${iterations})` : "";
      const passInfo = passed === true ? " ✓" : passed === false ? " ✗" : "";
      const statusLine = `${wfState || status}${phaseInfo}${iterInfo}${passInfo}`;

      // Only print when something changed
      if (statusLine !== lastState) {
        const time = new Date().toLocaleTimeString();
        console.error(`  [${time}] ${statusLine}`);
        lastState = statusLine;
      }

      if (phaseName && phaseName !== lastPhase) {
        lastPhase = phaseName;
      }

      // Terminal states
      if (["complete", "completed", "failed", "stopped", "error"].includes(status)) {
        console.error(`\nWorkflow ${status}.`);

        // Fetch final summary
        try {
          const summaryRes = await fetch(`${baseUrl}/task-runs/${taskRunId}`);
          if (summaryRes.ok) {
            const summaryData = await summaryRes.json();
            const run = summaryData?.data ?? summaryData;
            const sessions = run.sessions_count ?? 0;
            const cost = run.total_cost_usd != null ? `$${run.total_cost_usd.toFixed(2)}` : "n/a";
            const tokens = run.total_tokens != null ? `${Math.round(run.total_tokens / 1000)}k` : "n/a";
            console.error(`  Sessions: ${sessions}, Tokens: ${tokens}, Cost: ${cost}`);
          }
        } catch { /* non-fatal */ }
        break;
      }
    } catch (err) {
      // Network error — runner might be restarting
      console.error(`  [${new Date().toLocaleTimeString()}] (connection lost, retrying...)`);
    }

    await sleep(5000);
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

// ─── Main ───────────────────────────────────────────────────────────────────

const phases = parsePlan(planContent);
if (phases.length === 0) {
  console.error("Error: No plan phases found in the content.");
  process.exit(1);
}

console.error(`Parsed: ${phases.length} phases, ${phases.reduce((s, p) => s + p.tasks.length, 0)} tasks`);
for (const p of phases) {
  console.error(`  Phase: ${p.name} (${p.tasks.length} tasks)`);
}

const workflow = buildWorkflow(phases);
console.error(`Built workflow: "${workflow.name}" with ${workflow.stages.length} stages`);

if (dryRun) {
  console.log(JSON.stringify(workflow, null, 2));
  process.exit(0);
}

// Execute
const url = `http://localhost:${port}/unified-workflows/execute-inline`;
console.error(`Executing on ${url}...`);

const response = await fetch(url, {
  method: "POST",
  headers: { "Content-Type": "application/json" },
  body: JSON.stringify(workflow),
});

if (!response.ok) {
  const text = await response.text();
  console.error(`Error ${response.status}: ${text}`);
  process.exit(1);
}

const result = await response.json();
const taskRunId = result?.data?.loop_result?.task_run_id
  ?? result?.data?.task_run_id
  ?? result?.task_run_id;

console.error("Workflow started successfully.");
if (taskRunId) {
  console.error(`Task run ID: ${taskRunId}`);
}

if (!follow) {
  console.log(JSON.stringify(result, null, 2));
} else if (taskRunId) {
  await followProgress(taskRunId);
} else {
  console.error("Warning: Could not extract task_run_id from response — cannot follow progress.");
  console.log(JSON.stringify(result, null, 2));
}
