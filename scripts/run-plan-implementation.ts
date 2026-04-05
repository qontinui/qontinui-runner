#!/usr/bin/env npx tsx
/**
 * run-plan-implementation.ts
 *
 * Standalone script that parses a markdown plan and executes it as a
 * plan implementation workflow on the runner. Does not depend on the
 * monorepo type system — uses inline parsing and workflow construction.
 *
 * Usage:
 *   npx tsx scripts/run-plan-implementation.ts <plan-file.md> [--port 9876] [--dry-run]
 *   echo "plan text" | npx tsx scripts/run-plan-implementation.ts - [--port 9876]
 *
 * The AI in a Claude Code session can call this to launch plan implementation
 * without needing the runner UI.
 */

import { readFileSync } from "fs";
import { randomUUID } from "crypto";

// ─── Args ───────────────────────────────────────────────────────────────────

const args = process.argv.slice(2);
let planFile = args.find((a) => !a.startsWith("--"));
const portFlag = args.indexOf("--port");
const port = portFlag >= 0 ? parseInt(args[portFlag + 1], 10) : 9876;
const dryRun = args.includes("--dry-run");

if (!planFile) {
  console.error("Usage: npx tsx scripts/run-plan-implementation.ts <plan-file.md> [--port 9876] [--dry-run]");
  process.exit(1);
}

let planContent: string;
if (planFile === "-") {
  planContent = readFileSync(0, "utf-8"); // stdin
} else {
  planContent = readFileSync(planFile, "utf-8");
}

// ─── Minimal plan parser ────────────────────────────────────────────────────
// Extracts phases and tasks from common markdown plan formats.

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

function parsePlan(text: string): PlanPhase[] {
  const lines = text.split("\n");
  const phases: PlanPhase[] = [];
  let currentPhase: PlanPhase | null = null;

  for (const line of lines) {
    const trimmed = line.trim();

    // Heading-based phases: ## Phase 1: Name, ## 1. Name, ## Name
    const headingMatch = trimmed.match(
      /^#{1,3}\s+(?:(?:Phase|Part|Step)\s+)?(\d+)[\.:)\-]?\s*(.+)/i,
    );
    if (headingMatch) {
      if (currentPhase) phases.push(currentPhase);
      const name = headingMatch[2].trim();
      currentPhase = {
        id: `phase-${phases.length}`,
        name,
        description: name,
        tasks: [],
      };
      continue;
    }

    // Numbered top-level: 1. Phase name or 1) Phase name (only if no current phase or at root level)
    const numberedMatch = trimmed.match(/^(\d+)[.)]\s+\*{0,2}(.+?)\*{0,2}$/);
    if (numberedMatch && !line.startsWith("  ") && !line.startsWith("\t")) {
      // Check if this looks like a phase header (not a sub-task)
      const name = numberedMatch[2].replace(/\*{1,2}/g, "").trim();
      if (currentPhase && currentPhase.tasks.length === 0) {
        // Previous "phase" had no tasks — it was probably a task, convert it
        // Actually, just push it and start new
      }
      if (currentPhase) phases.push(currentPhase);
      currentPhase = {
        id: `phase-${phases.length}`,
        name,
        description: name,
        tasks: [],
      };
      continue;
    }

    // Sub-items as tasks: - Task name, * Task name, - [ ] Task name
    const taskMatch = trimmed.match(/^[-*]\s+(?:\[.\]\s+)?(.+)/);
    if (taskMatch && currentPhase) {
      const taskName = taskMatch[1].replace(/\*{1,2}/g, "").trim();
      currentPhase.tasks.push({
        id: `task-${currentPhase.tasks.length}`,
        name: taskName.split(/[—–:]/).shift()?.trim() || taskName,
        description: taskName,
      });
      continue;
    }

    // Continuation text — append to phase description
    if (currentPhase && trimmed.length > 0 && !trimmed.startsWith("#")) {
      if (currentPhase.description === currentPhase.name) {
        currentPhase.description = trimmed;
      } else {
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

    // Implement stage
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

    // Review stage
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

    // Next-steps stage
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

  // Commit stage
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
console.error("Workflow started successfully.");
console.log(JSON.stringify(result, null, 2));
