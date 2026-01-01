/**
 * FindingsParser
 *
 * Stateful parser for extracting structured findings from AI output.
 * Handles multi-line finding blocks and legacy issue markers.
 *
 * This module is responsible for parsing ONLY - it does not handle:
 * - Storage (use FindingsTracker)
 * - Events (use FindingsTracker)
 * - Sessions (use FindingsTracker)
 *
 * Usage:
 * ```typescript
 * const parser = new FindingsParser();
 * const findings = parser.processText(aiOutput);
 * // When session ends:
 * parser.reset();
 * ```
 */

import type { FindingSeverity, ParsedFinding } from "../types/findings";

// ==================== Regex Patterns ====================

/**
 * Pattern to match the start of a finding block
 * Format: [FINDING:category:severity] or [FINDING:category:severity:needs_input]
 */
export const FINDING_START_PATTERN = /\[FINDING:(\w+):(\w+)(?::needs_input)?\]/;

/**
 * Pattern to match the end of a finding block
 */
export const FINDING_END_PATTERN = /\[\/FINDING\]/;

/**
 * Content field patterns within a finding block
 */
export const TITLE_PATTERN = /^Title:\s*(.+)$/m;
export const DESCRIPTION_PATTERN =
  /^Description:\s*([\s\S]*?)(?=^(?:Title|File|Line|Question|Options|Resolution|Context):|\[\/FINDING\]|$)/m;
export const FILE_PATTERN = /^File:\s*(.+)$/m;
export const LINE_PATTERN = /^Line:\s*(\d+)$/m;
export const QUESTION_PATTERN = /^Question:\s*(.+)$/m;
export const OPTIONS_PATTERN = /^Options:\s*(.+)$/m;
export const RESOLUTION_PATTERN = /^Resolution:\s*(.+)$/m;

/**
 * Legacy issue marker pattern (for backward compatibility)
 */
export const LEGACY_ISSUE_DETECTED_PATTERN = /\[ISSUE:DETECTED\]/;

// ==================== Types ====================

/**
 * Current state of the parser (for debugging/introspection)
 */
export interface ParsingState {
  isBuffering: boolean;
  bufferContent: string;
  currentMeta: {
    categoryId: string;
    severity: FindingSeverity;
    needsInput: boolean;
  } | null;
}

/**
 * Internal metadata for tracking finding parsing state
 */
interface FindingParsingMeta {
  categoryId: string;
  severity: FindingSeverity;
  needsInput: boolean;
}

// ==================== Utility Functions ====================

/**
 * Parse severity string to typed severity
 * Returns "medium" as default for invalid values
 */
export function parseSeverity(severity: string): FindingSeverity {
  const validSeverities: FindingSeverity[] = ["critical", "high", "medium", "low", "info"];
  const lower = severity.toLowerCase() as FindingSeverity;
  return validSeverities.includes(lower) ? lower : "medium";
}

/**
 * Parse a finding block content into structured data
 *
 * @param content - The raw content between [FINDING:...] and [/FINDING]
 * @param categoryId - The category ID from the finding marker
 * @param severity - The severity level from the finding marker
 * @param needsInput - Whether this finding requires user input
 * @returns Parsed finding data
 */
export function parseBlockContent(
  content: string,
  categoryId: string,
  severity: FindingSeverity,
  needsInput: boolean,
): ParsedFinding {
  const titleMatch = content.match(TITLE_PATTERN);
  const descriptionMatch = content.match(DESCRIPTION_PATTERN);
  const fileMatch = content.match(FILE_PATTERN);
  const lineMatch = content.match(LINE_PATTERN);
  const questionMatch = content.match(QUESTION_PATTERN);
  const optionsMatch = content.match(OPTIONS_PATTERN);
  const resolutionMatch = content.match(RESOLUTION_PATTERN);

  return {
    categoryId,
    severity,
    title: titleMatch?.[1]?.trim() ?? "Untitled Finding",
    description: descriptionMatch?.[1]?.trim() ?? content.trim(),
    needsInput,
    question: questionMatch?.[1]?.trim(),
    options: optionsMatch?.[1]?.split("|").map((o) => o.trim()),
    file: fileMatch?.[1]?.trim(),
    line: lineMatch ? parseInt(lineMatch[1], 10) : undefined,
    resolution: resolutionMatch?.[1]?.trim(),
  };
}

// ==================== FindingsParser Class ====================

/**
 * Stateful parser for extracting findings from AI output
 *
 * The parser maintains a buffer for multi-line finding blocks.
 * When a [FINDING:...] marker is detected, the parser starts buffering.
 * When [/FINDING] is detected, the buffer is parsed and a finding is returned.
 *
 * Important: The parser handles text that may contain multiple lines
 * separated by \n (Claude often sends entire paragraphs as single chunks).
 */
export class FindingsParser {
  /** Buffer for accumulating multi-line finding content */
  private parsingBuffer: string = "";

  /** Metadata for the current finding being parsed */
  private currentFindingMeta: FindingParsingMeta | null = null;

  /**
   * Process text that may contain multiple lines separated by \n
   *
   * Claude often sends entire paragraphs with embedded \n characters.
   * This method splits by newlines and processes each sub-line individually,
   * collecting any findings that are completed.
   *
   * @param text - The text to process (may contain \n)
   * @returns Array of parsed findings (can be 0, 1, or more)
   */
  processText(text: string): ParsedFinding[] {
    const lines = text.split("\n");
    const findings: ParsedFinding[] = [];

    for (const line of lines) {
      const finding = this.processSingleLine(line);
      if (finding) {
        findings.push(finding);
      }
    }

    return findings;
  }

  /**
   * Process a single line of AI output for finding markers
   *
   * This method handles:
   * 1. Continuation of a multi-line finding block (buffering)
   * 2. End of a finding block ([/FINDING])
   * 3. Start of a new finding block ([FINDING:...])
   * 4. Single-line findings (start and end on same line)
   * 5. Legacy [ISSUE:DETECTED] markers
   *
   * @param line - A single line of text (no newlines)
   * @returns Parsed finding if one was completed, null otherwise
   */
  processSingleLine(line: string): ParsedFinding | null {
    // Check if we're currently parsing a multi-line finding block
    if (this.currentFindingMeta) {
      // Check for end marker
      if (FINDING_END_PATTERN.test(line)) {
        // Parse the accumulated buffer
        const parsed = parseBlockContent(
          this.parsingBuffer,
          this.currentFindingMeta.categoryId,
          this.currentFindingMeta.severity,
          this.currentFindingMeta.needsInput,
        );

        // Reset parsing state
        this.parsingBuffer = "";
        this.currentFindingMeta = null;

        return parsed;
      } else {
        // Accumulate content
        this.parsingBuffer += line + "\n";
        return null;
      }
    }

    // Check for start of new finding block
    const startMatch = line.match(FINDING_START_PATTERN);
    if (startMatch) {
      const [, categoryId, severity] = startMatch;
      const needsInput = line.includes(":needs_input]");

      // Start buffering
      this.currentFindingMeta = {
        categoryId,
        severity: parseSeverity(severity),
        needsInput,
      };

      // Get content after the marker on this line
      const markerEnd = line.indexOf("]") + 1;
      const restOfLine = line.substring(markerEnd).trim();

      // Check if this is a single-line finding (has end marker on same line)
      if (FINDING_END_PATTERN.test(restOfLine)) {
        const content = restOfLine.replace(FINDING_END_PATTERN, "").trim();
        const parsed = parseBlockContent(content, categoryId, parseSeverity(severity), needsInput);
        this.currentFindingMeta = null;
        return parsed;
      }

      this.parsingBuffer = restOfLine + "\n";
      return null;
    }

    // Check for legacy [ISSUE:DETECTED] markers (backward compatibility)
    if (LEGACY_ISSUE_DETECTED_PATTERN.test(line)) {
      const contextAfterMarker = line.split("[ISSUE:DETECTED]")[1]?.trim() || "";
      // Remove any JSON-like content that might follow
      const cleanContext = contextAfterMarker.replace(/^\s*\{[\s\S]*$/, "").trim();

      return {
        categoryId: "code_bug",
        severity: "medium",
        title: cleanContext || "Issue detected",
        description: line,
        needsInput: false,
      };
    }

    return null;
  }

  /**
   * Get the current parsing state (for debugging/introspection)
   *
   * Useful for understanding what the parser is currently doing,
   * especially when debugging multi-line finding parsing.
   *
   * @returns Current parsing state
   */
  getParsingState(): ParsingState {
    return {
      isBuffering: this.currentFindingMeta !== null,
      bufferContent: this.parsingBuffer,
      currentMeta: this.currentFindingMeta
        ? {
            categoryId: this.currentFindingMeta.categoryId,
            severity: this.currentFindingMeta.severity,
            needsInput: this.currentFindingMeta.needsInput,
          }
        : null,
    };
  }

  /**
   * Reset the parser state
   *
   * Call this when a session ends to clear any partial finding
   * that may have been in progress.
   */
  reset(): void {
    this.parsingBuffer = "";
    this.currentFindingMeta = null;
  }

  /**
   * Check if the parser is currently buffering a multi-line finding
   *
   * @returns true if a finding block is in progress
   */
  isBuffering(): boolean {
    return this.currentFindingMeta !== null;
  }
}
