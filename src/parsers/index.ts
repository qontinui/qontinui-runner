/**
 * Parsers Module
 *
 * This module contains parsers for extracting structured data from various formats.
 */

export {
  FindingsParser,
  parseSeverity,
  parseBlockContent,
  // Regex patterns (exported for testing/advanced use)
  FINDING_START_PATTERN,
  FINDING_END_PATTERN,
  TITLE_PATTERN,
  DESCRIPTION_PATTERN,
  FILE_PATTERN,
  LINE_PATTERN,
  QUESTION_PATTERN,
  OPTIONS_PATTERN,
  RESOLUTION_PATTERN,
  LEGACY_ISSUE_DETECTED_PATTERN,
  // Types
  type ParsingState,
} from "./FindingsParser";
