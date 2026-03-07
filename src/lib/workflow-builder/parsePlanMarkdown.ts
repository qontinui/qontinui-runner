/**
 * parsePlanMarkdown
 *
 * Parses free-form markdown plan text (from AI sessions or PLAN*.md files)
 * into structured PlanPhase[] that buildPlanWorkflow can consume.
 *
 * Handles common plan formats:
 *   - Heading-based phases (## Phase 1: Name)
 *   - Numbered phases (1. Phase name)
 *   - Checklist format (- [ ] Phase: ...)
 *   - Tasks as sub-items under phases
 *   - Verification hints from keywords (compile, test, curl, UI, etc.)
 */

import type { PlanPhase, PlanTask, TaskVerification, VerificationType } from "./buildPlanWorkflow";

// =============================================================================
// Verification inference from text
// =============================================================================

const COMPILE_PATTERNS = [
  /\btsc\b/i,
  /\btypecheck\b/i,
  /\btype.?check\b/i,
  /\bcargo\s+check\b/i,
  /\bcargo\s+build\b/i,
  /\bcompile[sd]?\b/i,
  /\bnpx\s+tsc\b/i,
  /\b--noEmit\b/,
];

const API_CHECK_PATTERNS = [
  /\bcurl\b/i,
  /\bhttp[s]?:\/\//i,
  /\breturns?\s+\d{3}\b/i,
  /\bstatus\s+\d{3}\b/i,
  /\bendpoint\s+respond/i,
  /\bAPI\s+respond/i,
  /\bGET\s+\//,
  /\bPOST\s+\//,
  /\bPUT\s+\//,
  /\bDELETE\s+\//,
];

const FILE_EXISTS_PATTERNS = [
  /\bfile\s+exists?\b/i,
  /\bshould\s+exist\b/i,
  /\btest\s+-[fed]\b/,
  /\bcreate[sd]?\s+file\b/i,
  /\bmigration\s+file\b/i,
];

const UI_CHECK_PATTERNS = [
  /\bUI\s+(shows?|displays?|renders?|contains?)\b/i,
  /\bvisible\b/i,
  /\bpage\s+(shows?|displays?|renders?|contains?)\b/i,
  /\belement\s+(exists?|visible|present)\b/i,
  /\bbutton\s+(exists?|visible|shows?)\b/i,
  /\bcomponent\s+(renders?|shows?|displays?)\b/i,
];

const COMMAND_PATTERNS = [
  /\brun\s+`[^`]+`/i,
  /\bexecute\s+`[^`]+`/i,
  /`[^`]*(?:npm|yarn|pnpm|cargo|python|pip|make)\s[^`]*`/,
  /\bnpm\s+(?:run|test)\b/i,
  /\byarn\s+(?:run|test)\b/i,
  /\bcargo\s+test\b/i,
];

/** Extract a command from backticks in text, if present. */
function extractCommand(text: string): string | undefined {
  const match = text.match(/`([^`]{3,})`/);
  return match?.[1];
}

/** Extract a URL from text, if present. */
function extractUrl(text: string): string | undefined {
  const match = text.match(/https?:\/\/[^\s)]+/);
  return match?.[0];
}

/** Extract expected HTTP status code from text. */
function extractStatusCode(text: string): number | undefined {
  const match = text.match(/(?:returns?|status)\s+(\d{3})/i);
  return match ? parseInt(match[1], 10) : undefined;
}

/** Extract a file path from text. */
function extractFilePath(text: string): string | undefined {
  // Look for paths in backticks first
  const backtickMatch = text.match(/`([^`]*\/[^`]+)`/);
  if (backtickMatch) return backtickMatch[1];
  // Look for common path patterns
  const pathMatch = text.match(/(?:^|\s)((?:src|lib|app|components|pages)\/[\w/./-]+)/);
  return pathMatch?.[1];
}

/** Infer verification type and details from a description line. */
function inferVerification(text: string): TaskVerification {
  const command = extractCommand(text);

  // Check patterns in priority order
  for (const pattern of COMPILE_PATTERNS) {
    if (pattern.test(text)) {
      return {
        type: "compile",
        description: text,
        compile_command: command,
      };
    }
  }

  for (const pattern of API_CHECK_PATTERNS) {
    if (pattern.test(text)) {
      return {
        type: "api_check",
        description: text,
        url: extractUrl(text),
        expected_status: extractStatusCode(text),
      };
    }
  }

  for (const pattern of FILE_EXISTS_PATTERNS) {
    if (pattern.test(text)) {
      return {
        type: "file_exists",
        description: text,
        file_path: extractFilePath(text),
      };
    }
  }

  for (const pattern of UI_CHECK_PATTERNS) {
    if (pattern.test(text)) {
      return {
        type: "ui_check",
        description: text,
        assertions: [
          {
            id: crypto.randomUUID(),
            description: text,
            assertionType: "exists",
            criteria: {
              textContent: text
                .replace(
                  /\b(UI|page|element|button|component)\s+(shows?|displays?|renders?|contains?|exists?|visible|present)\b/gi,
                  "",
                )
                .trim()
                .slice(0, 50),
            },
          },
        ],
      };
    }
  }

  for (const pattern of COMMAND_PATTERNS) {
    if (pattern.test(text)) {
      return {
        type: "command",
        description: text,
        command,
      };
    }
  }

  // If we found a command in backticks, treat as a command verification
  if (command) {
    return {
      type: "command",
      description: text,
      command,
    };
  }

  // Default to AI review
  return {
    type: "ai_review",
    description: text,
    criteria: text,
  };
}

// =============================================================================
// Plan structure parsing
// =============================================================================

interface ParsedLine {
  depth: number;
  text: string;
  isHeading: boolean;
  headingLevel: number;
  isCheckbox: boolean;
  isNumbered: boolean;
  number?: number;
}

function parseLine(raw: string): ParsedLine {
  // Calculate indentation depth
  const leadingSpaces = raw.match(/^(\s*)/)?.[1] ?? "";
  const depth = leadingSpaces.replace(/\t/g, "  ").length;
  let text = raw.trim();

  // Detect heading
  const headingMatch = text.match(/^(#{1,6})\s+(.+)/);
  if (headingMatch) {
    return {
      depth: 0,
      text: headingMatch[2],
      isHeading: true,
      headingLevel: headingMatch[1].length,
      isCheckbox: false,
      isNumbered: false,
    };
  }

  // Strip list markers
  const checkboxMatch = text.match(/^[-*]\s+\[[ x?]\]\s+(.*)/i);
  if (checkboxMatch) {
    return {
      depth,
      text: checkboxMatch[1],
      isHeading: false,
      headingLevel: 0,
      isCheckbox: true,
      isNumbered: false,
    };
  }

  const numberedMatch = text.match(/^(\d+)[.)]\s+(.*)/);
  if (numberedMatch) {
    return {
      depth,
      text: numberedMatch[2],
      isHeading: false,
      headingLevel: 0,
      isCheckbox: false,
      isNumbered: true,
      number: parseInt(numberedMatch[1], 10),
    };
  }

  const bulletMatch = text.match(/^[-*+]\s+(.*)/);
  if (bulletMatch) {
    text = bulletMatch[1];
  }

  return {
    depth,
    text,
    isHeading: false,
    headingLevel: 0,
    isCheckbox: false,
    isNumbered: false,
  };
}

/** Detect if a line is a verification hint. */
function isVerificationLine(text: string): boolean {
  return (
    /^(?:verification|verify|check|test|assert|validate|confirm|ensure)[\s:]/i.test(text) ||
    /^\*\*(?:verification|verify|check|test)\*\*/i.test(text)
  );
}

/** Strip verification prefix from text. */
function stripVerificationPrefix(text: string): string {
  return text
    .replace(/^\*\*(?:verification|verify|check|test)\*\*[:\s]*/i, "")
    .replace(/^(?:verification|verify|check|test)[:\s]*/i, "")
    .trim();
}

/**
 * Analyze the heading hierarchy to determine which level represents phases vs tasks.
 * Returns { phaseLevel, taskLevel } where taskLevel may be null if only one heading level exists.
 */
function detectHeadingHierarchy(lines: ParsedLine[]): {
  phaseLevel: number;
  taskLevel: number | null;
} {
  const headingLevels = new Set<number>();
  for (const line of lines) {
    if (line.isHeading) headingLevels.add(line.headingLevel);
  }

  // Remove level 1 (document title) from consideration
  headingLevels.delete(1);

  const sorted = [...headingLevels].sort((a, b) => a - b);

  if (sorted.length === 0) {
    // No headings — will use numbered/bulleted items
    return { phaseLevel: 99, taskLevel: null };
  }
  if (sorted.length === 1) {
    // Single heading level — all are phases, tasks come from list items
    return { phaseLevel: sorted[0], taskLevel: null };
  }
  // Two or more levels — topmost = phases, next = tasks
  return { phaseLevel: sorted[0], taskLevel: sorted[1] };
}

/** Detect if text looks like a phase/section header at the given heading level. */
function isPhaseHeader(line: ParsedLine, phaseLevel: number): boolean {
  if (line.isHeading && line.headingLevel === phaseLevel) return true;
  // "Phase 1: ...", "Step 1: ..."
  if (/^(?:phase|step|stage|part)\s+\d/i.test(line.text)) return true;
  // Top-level numbered items with a colon (only when no headings detected)
  if (phaseLevel >= 99 && line.isNumbered && line.depth === 0 && line.text.includes(":"))
    return true;
  return false;
}

/** Detect if text looks like a task under a phase. */
function isTaskLine(line: ParsedLine, taskLevel: number | null): boolean {
  // Sub-headings at the task level
  if (taskLevel !== null && line.isHeading && line.headingLevel === taskLevel) return true;
  // Numbered or bulleted items (when no task-level headings exist)
  if (taskLevel === null && (line.isNumbered || line.isCheckbox)) return true;
  // Any heading below the task level is still a task
  if (taskLevel !== null && line.isHeading && line.headingLevel > taskLevel) return true;
  return false;
}

// =============================================================================
// Main parser
// =============================================================================

/**
 * Parse markdown plan text into PlanPhase[].
 *
 * The parser uses a two-pass approach:
 * 1. Split the document into phase blocks based on headings/structure
 * 2. Within each phase, extract tasks and their verifications
 */
export function parsePlanMarkdown(markdown: string): PlanPhase[] {
  const lines = markdown.split("\n");
  const parsed = lines.map(parseLine);

  // Skip empty lines and find content boundaries
  const contentLines = parsed
    .map((p, i) => ({ ...p, lineIndex: i }))
    .filter((p) => p.text.length > 0);

  if (contentLines.length === 0) return [];

  // Detect heading hierarchy for proper phase/task separation
  const { phaseLevel, taskLevel } = detectHeadingHierarchy(contentLines);

  // Pass 1: Identify phase boundaries (skip document title at h1)
  const phaseBoundaries: { start: number; name: string; description: string }[] = [];

  for (let i = 0; i < contentLines.length; i++) {
    const line = contentLines[i];
    // Skip h1 document titles
    if (line.isHeading && line.headingLevel === 1) continue;
    if (isPhaseHeader(line, phaseLevel)) {
      // Clean phase name: remove "Phase 1:", numbering, etc.
      let name = line.text
        .replace(/^(?:phase|step|stage|part)\s+\d+[.:]\s*/i, "")
        .replace(/^\d+[.:]\s*/, "")
        .trim();
      if (!name) name = line.text;

      phaseBoundaries.push({
        start: i,
        name,
        description: "",
      });
    }
  }

  // If no phase boundaries found, treat the entire document as one phase
  if (phaseBoundaries.length === 0) {
    phaseBoundaries.push({
      start: 0,
      name: "Implementation",
      description: "Parsed from plan",
    });
  }

  // Pass 2: Extract tasks and verifications within each phase
  const phases: PlanPhase[] = [];

  for (let p = 0; p < phaseBoundaries.length; p++) {
    const phaseStart = phaseBoundaries[p].start;
    const phaseEnd =
      p + 1 < phaseBoundaries.length ? phaseBoundaries[p + 1].start : contentLines.length;
    const phaseLines = contentLines.slice(phaseStart + 1, phaseEnd);

    const tasks: PlanTask[] = [];
    let currentTask: {
      name: string;
      description: string;
      verifications: TaskVerification[];
    } | null = null;

    for (const line of phaseLines) {
      // Check if this is a verification line
      if (isVerificationLine(line.text)) {
        const cleanText = stripVerificationPrefix(line.text);
        const verification = inferVerification(cleanText);

        if (currentTask) {
          currentTask.verifications.push(verification);
        } else {
          // Verification without a task — create an implicit task
          currentTask = {
            name: cleanText.slice(0, 60),
            description: cleanText,
            verifications: [verification],
          };
        }
        continue;
      }

      // Check if this looks like a new task
      if (isTaskLine(line, taskLevel) && !isVerificationLine(line.text)) {
        // Save previous task
        if (currentTask) {
          tasks.push({
            id: crypto.randomUUID(),
            ...currentTask,
          });
        }

        // Start new task
        let name = line.text.replace(/^\d+[.:]\s*/, "").trim();
        if (!name) name = line.text;

        currentTask = {
          name: name.slice(0, 80),
          description: name,
          verifications: [],
        };
        continue;
      }

      // Additional description line for current task
      if (currentTask && line.text.trim()) {
        // Check if it contains verification hints even without explicit prefix
        const lowerText = line.text.toLowerCase();
        if (
          lowerText.includes("should compile") ||
          lowerText.includes("should pass") ||
          lowerText.includes("should return") ||
          lowerText.includes("should show") ||
          lowerText.includes("should exist") ||
          lowerText.includes("must compile") ||
          lowerText.includes("must pass")
        ) {
          currentTask.verifications.push(inferVerification(line.text));
        }
      }
    }

    // Save last task
    if (currentTask) {
      tasks.push({
        id: crypto.randomUUID(),
        ...currentTask,
      });
    }

    // If no tasks found, create one from the phase description
    if (tasks.length === 0) {
      tasks.push({
        id: crypto.randomUUID(),
        name: phaseBoundaries[p].name,
        description: `Implement: ${phaseBoundaries[p].name}`,
        verifications: [
          {
            type: "ai_review",
            description: `Verify ${phaseBoundaries[p].name} is complete`,
            criteria: `Review the implementation and verify that "${phaseBoundaries[p].name}" has been completed successfully.`,
          },
        ],
      });
    }

    // Ensure every task has at least one verification
    for (const task of tasks) {
      if (task.verifications.length === 0) {
        task.verifications.push({
          type: "ai_review",
          description: `Verify: ${task.name}`,
          criteria: `Review and verify that "${task.name}" has been implemented correctly.`,
        });
      }
    }

    phases.push({
      id: crypto.randomUUID(),
      name: phaseBoundaries[p].name,
      description: phaseLines
        .map((l) => l.text)
        .join(" ")
        .slice(0, 200),
      tasks,
    });
  }

  return phases;
}

/** Summarize parsing results for display. */
export function summarizeParsedPlan(phases: PlanPhase[]): {
  phaseCount: number;
  taskCount: number;
  verificationCount: number;
  deterministicCount: number;
  aiCount: number;
  byType: Record<VerificationType, number>;
} {
  const byType: Record<VerificationType, number> = {
    compile: 0,
    ui_check: 0,
    command: 0,
    file_exists: 0,
    api_check: 0,
    ai_review: 0,
  };

  let taskCount = 0;
  let verificationCount = 0;

  for (const phase of phases) {
    taskCount += phase.tasks.length;
    for (const task of phase.tasks) {
      for (const v of task.verifications) {
        verificationCount++;
        byType[v.type]++;
      }
    }
  }

  const aiCount = byType.ai_review;
  const deterministicCount = verificationCount - aiCount;

  return {
    phaseCount: phases.length,
    taskCount,
    verificationCount,
    deterministicCount,
    aiCount,
    byType,
  };
}
