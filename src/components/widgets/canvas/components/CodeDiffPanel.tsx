/**
 * CodeDiffPanel Component
 *
 * Renders a unified diff view with basic syntax coloring
 * and lightweight language-aware token highlighting.
 */

import { cn } from "@/lib/utils";
import type { CanvasPanelComponentProps } from "./CanvasComponentRegistry";

// Language keyword sets for syntax highlighting
const KEYWORD_SETS: Record<string, Set<string>> = {
  typescript: new Set([
    "import",
    "export",
    "from",
    "const",
    "let",
    "var",
    "function",
    "return",
    "if",
    "else",
    "for",
    "while",
    "class",
    "interface",
    "type",
    "extends",
    "implements",
    "new",
    "this",
    "async",
    "await",
    "try",
    "catch",
    "throw",
    "switch",
    "case",
    "default",
    "break",
    "continue",
    "typeof",
    "instanceof",
    "in",
    "of",
    "null",
    "undefined",
    "true",
    "false",
    "void",
    "never",
    "any",
    "string",
    "number",
    "boolean",
    "enum",
    "abstract",
    "static",
    "readonly",
    "private",
    "public",
    "protected",
    "as",
    "is",
    "keyof",
    "declare",
  ]),
  javascript: new Set([
    "import",
    "export",
    "from",
    "const",
    "let",
    "var",
    "function",
    "return",
    "if",
    "else",
    "for",
    "while",
    "class",
    "extends",
    "new",
    "this",
    "async",
    "await",
    "try",
    "catch",
    "throw",
    "switch",
    "case",
    "default",
    "break",
    "continue",
    "typeof",
    "instanceof",
    "in",
    "of",
    "null",
    "undefined",
    "true",
    "false",
    "void",
    "yield",
    "delete",
    "super",
  ]),
  python: new Set([
    "import",
    "from",
    "def",
    "class",
    "return",
    "if",
    "elif",
    "else",
    "for",
    "while",
    "try",
    "except",
    "finally",
    "raise",
    "with",
    "as",
    "pass",
    "break",
    "continue",
    "and",
    "or",
    "not",
    "is",
    "in",
    "True",
    "False",
    "None",
    "lambda",
    "yield",
    "async",
    "await",
    "global",
    "nonlocal",
    "assert",
    "del",
    "self",
    "cls",
  ]),
  rust: new Set([
    "fn",
    "let",
    "mut",
    "const",
    "static",
    "struct",
    "enum",
    "impl",
    "trait",
    "type",
    "use",
    "mod",
    "pub",
    "crate",
    "super",
    "self",
    "if",
    "else",
    "for",
    "while",
    "loop",
    "match",
    "return",
    "break",
    "continue",
    "async",
    "await",
    "move",
    "ref",
    "where",
    "as",
    "in",
    "true",
    "false",
    "unsafe",
    "extern",
    "dyn",
    "box",
  ]),
  go: new Set([
    "func",
    "var",
    "const",
    "type",
    "struct",
    "interface",
    "package",
    "import",
    "return",
    "if",
    "else",
    "for",
    "range",
    "switch",
    "case",
    "default",
    "break",
    "continue",
    "go",
    "defer",
    "select",
    "chan",
    "map",
    "make",
    "new",
    "nil",
    "true",
    "false",
    "error",
    "string",
    "int",
    "bool",
    "byte",
    "float64",
    "append",
    "len",
    "cap",
  ]),
};

// Alias common language names
const LANGUAGE_ALIASES: Record<string, string> = {
  ts: "typescript",
  tsx: "typescript",
  js: "javascript",
  jsx: "javascript",
  py: "python",
  rs: "rust",
};

/**
 * Apply lightweight syntax highlighting to a code line.
 * Returns JSX elements with colored spans for keywords, strings, numbers, and comments.
 */
function highlightLine(text: string, language: string): React.ReactNode {
  const normalizedLang = LANGUAGE_ALIASES[language] || language;
  const keywords = KEYWORD_SETS[normalizedLang];

  if (!keywords) {
    return text;
  }

  // Tokenize the line into segments
  const segments: React.ReactNode[] = [];
  let remaining = text;
  let key = 0;

  while (remaining.length > 0) {
    // Check for line comments
    const commentMatch = remaining.match(/^(\/\/.*|#.*|--.*)/);
    if (commentMatch) {
      segments.push(
        <span key={key++} className="text-muted-foreground italic">
          {commentMatch[0]}
        </span>,
      );
      remaining = remaining.slice(commentMatch[0].length);
      continue;
    }

    // Check for strings (single or double quoted)
    const stringMatch = remaining.match(/^("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|`(?:[^`\\]|\\.)*`)/);
    if (stringMatch) {
      segments.push(
        <span key={key++} className="text-amber-400">
          {stringMatch[0]}
        </span>,
      );
      remaining = remaining.slice(stringMatch[0].length);
      continue;
    }

    // Check for numbers
    const numberMatch = remaining.match(/^(\b\d+\.?\d*(?:e[+-]?\d+)?\b)/i);
    if (numberMatch) {
      segments.push(
        <span key={key++} className="text-cyan-400">
          {numberMatch[0]}
        </span>,
      );
      remaining = remaining.slice(numberMatch[0].length);
      continue;
    }

    // Check for keywords (word boundary)
    const wordMatch = remaining.match(/^(\b[a-zA-Z_]\w*\b)/);
    if (wordMatch) {
      const word = wordMatch[0];
      if (keywords.has(word)) {
        segments.push(
          <span key={key++} className="text-purple-400">
            {word}
          </span>,
        );
      } else {
        segments.push(word);
      }
      remaining = remaining.slice(word.length);
      continue;
    }

    // Non-matching character — advance by one
    segments.push(remaining[0]);
    remaining = remaining.slice(1);
  }

  return segments.length > 0 ? <>{segments}</> : text;
}

export function CodeDiffPanel({ data }: CanvasPanelComponentProps) {
  const filePath = (data.file_path as string) ?? "";
  const language = (data.language as string) ?? "";
  const unifiedDiff = (data.unified_diff as string) ?? "";
  const oldContent = (data.old_content as string) ?? "";
  const newContent = (data.new_content as string) ?? "";

  // Prefer unified diff if provided
  const diffText = unifiedDiff || generateSimpleDiff(oldContent, newContent);

  if (!diffText && !oldContent && !newContent) {
    return <p className="text-sm text-muted-foreground italic">No diff data</p>;
  }

  const lines = diffText.split("\n");

  return (
    <div className="space-y-2">
      {/* File path header */}
      {filePath && (
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <span className="font-mono">{filePath}</span>
          {language && <span className="text-[10px] px-1.5 py-0 bg-muted rounded">{language}</span>}
        </div>
      )}

      {/* Diff content */}
      <pre className="text-xs font-mono overflow-auto bg-muted/30 rounded p-2">
        {lines.map((line, idx) => {
          const isAdd = line.startsWith("+") && !line.startsWith("+++");
          const isDel = line.startsWith("-") && !line.startsWith("---");
          const isHunk = line.startsWith("@@");
          const isMeta = line.startsWith("diff ");

          // Extract the code portion (skip the +/- prefix for highlighting)
          const codeText = isAdd || isDel ? line.slice(1) : line;
          const prefix = isAdd ? "+" : isDel ? "-" : "";

          return (
            <div
              key={idx}
              className={cn(
                "flex",
                isAdd && "bg-green-500/10 text-green-400",
                isDel && "bg-red-500/10 text-red-400",
                isHunk && "text-cyan-400",
                isMeta && "text-muted-foreground font-bold",
              )}
            >
              <span className="w-8 flex-shrink-0 text-right pr-2 text-muted-foreground/40 select-none">
                {idx + 1}
              </span>
              <span className="px-1">
                {isHunk || isMeta ? (
                  line
                ) : (
                  <>
                    {prefix}
                    {highlightLine(codeText, language)}
                  </>
                )}
              </span>
            </div>
          );
        })}
      </pre>
    </div>
  );
}

/**
 * Generate a simple diff when only old/new content is provided.
 */
function generateSimpleDiff(oldContent: string, newContent: string): string {
  if (!oldContent && !newContent) return "";

  const lines: string[] = [];
  if (oldContent) {
    for (const line of oldContent.split("\n")) {
      lines.push(`- ${line}`);
    }
  }
  if (newContent) {
    for (const line of newContent.split("\n")) {
      lines.push(`+ ${line}`);
    }
  }
  return lines.join("\n");
}
