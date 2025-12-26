/**
 * LearningsSubTab.tsx
 *
 * Allows users to analyze completed AI workflow loops to extract learnings.
 * AI reviews what worked, what didn't, and identifies patterns that can be
 * added to CLAUDE.md, skills, or agent configurations.
 */

import { useState, useMemo } from "react";
import {
  Lightbulb,
  Play,
  Copy,
  FileText,
  Clock,
  CheckCircle,
  AlertCircle,
  Loader2,
  ChevronDown,
  ChevronUp,
  Sparkles,
  BookOpen,
} from "lucide-react";
import type { AiOutputLine } from "../AiOutputTab";
import { groupEntriesIntoLoops, type AiLoop } from "../../types/aiLoop";

interface LearningsSubTabProps {
  aiOutputLines: AiOutputLine[];
}

interface Learning {
  id: string;
  loopId: string;
  loopLabel: string;
  generatedAt: number;
  content: string;
  summary: string;
}

export function LearningsSubTab({ aiOutputLines }: LearningsSubTabProps) {
  const [selectedLoopId, setSelectedLoopId] = useState<string | null>(null);
  const [isAnalyzing, setIsAnalyzing] = useState(false);
  const [learnings, setLearnings] = useState<Learning[]>([]);
  const [expandedLearningId, setExpandedLearningId] = useState<string | null>(null);
  const [copySuccess, setCopySuccess] = useState<string | null>(null);

  // Group entries into loops, sorted by most recent first
  const loops = useMemo(() => {
    const grouped = groupEntriesIntoLoops(aiOutputLines);
    return [...grouped].reverse(); // Most recent first
  }, [aiOutputLines]);

  const selectedLoop = loops.find((l) => l.id === selectedLoopId);

  // Format timestamp for display
  const formatTime = (timestamp: number) => {
    const date = new Date(timestamp);
    return date.toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  };

  // Get full text content of a loop for analysis
  const getLoopContent = (loop: AiLoop): string => {
    return loop.entries
      .map((e) => {
        const prefix = e.source === "prompt" ? "## User Prompt\n" : "";
        return prefix + e.line;
      })
      .join("\n\n");
  };

  // Analyze selected loop
  const analyzeLoop = async () => {
    if (!selectedLoop) return;

    setIsAnalyzing(true);
    try {
      const loopContent = getLoopContent(selectedLoop);

      const analysisPrompt = `You are analyzing an AI workflow session to extract learnings that can improve future sessions.

## Session Content
${loopContent}

## Your Task
Analyze this AI workflow session and provide:

1. **Summary**: Brief 1-2 sentence summary of what this session accomplished
2. **What Worked Well**: Patterns, approaches, or techniques that were effective
3. **What Could Be Improved**: Areas where the AI struggled or could have done better
4. **Project-Specific Patterns**: Any patterns specific to this codebase that should be remembered
5. **Suggested CLAUDE.md Addition**: If appropriate, provide text that could be added to CLAUDE.md to help future sessions

Format your response as markdown with clear sections.`;

      const response = await fetch("http://localhost:9876/trigger-ai-analysis", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          prompt: analysisPrompt,
          context: "learnings_analysis",
        }),
      });

      const result = await response.json();

      if (result.success) {
        // Create a new learning entry
        const newLearning: Learning = {
          id: `learning-${Date.now()}`,
          loopId: selectedLoop.id,
          loopLabel: selectedLoop.label,
          generatedAt: Date.now(),
          content: result.data?.response || "Analysis completed. Check AI Output tab for results.",
          summary: selectedLoop.promptPreview,
        };

        setLearnings((prev) => [newLearning, ...prev]);
        setExpandedLearningId(newLearning.id);
      } else {
        throw new Error(result.error || "Failed to analyze loop");
      }
    } catch (error) {
      console.error("Failed to analyze loop:", error);
      // Still create an entry pointing to AI Output
      const newLearning: Learning = {
        id: `learning-${Date.now()}`,
        loopId: selectedLoop.id,
        loopLabel: selectedLoop.label,
        generatedAt: Date.now(),
        content: "Analysis triggered. Check the AI Output tab for the generated learnings.",
        summary: selectedLoop.promptPreview,
      };
      setLearnings((prev) => [newLearning, ...prev]);
      setExpandedLearningId(newLearning.id);
    } finally {
      setIsAnalyzing(false);
    }
  };

  // Copy learning content to clipboard
  const copyToClipboard = async (content: string, id: string) => {
    try {
      await navigator.clipboard.writeText(content);
      setCopySuccess(id);
      setTimeout(() => setCopySuccess(null), 2000);
    } catch (error) {
      console.error("Failed to copy:", error);
    }
  };

  // Calculate loop duration
  const getLoopDuration = (loop: AiLoop): string => {
    const durationMs = loop.endTime - loop.startTime;
    const seconds = Math.floor(durationMs / 1000);
    if (seconds < 60) return `${seconds}s`;
    const minutes = Math.floor(seconds / 60);
    const remainingSeconds = seconds % 60;
    return `${minutes}m ${remainingSeconds}s`;
  };

  return (
    <div className="h-full p-4 flex flex-col gap-4">
      {/* Header */}
      <div className="flex items-center gap-3">
        <Lightbulb className="w-6 h-6 text-yellow-500" />
        <div>
          <h2 className="text-xl font-semibold">Learnings</h2>
          <p className="text-sm text-muted-foreground">
            Analyze AI workflow sessions to extract patterns and improve future runs
          </p>
        </div>
      </div>

      <div className="flex-1 flex gap-4 min-h-0">
        {/* Left Panel: Loop Selection */}
        <div className="w-80 flex flex-col border border-border rounded-lg overflow-hidden">
          <div className="p-3 border-b border-border bg-muted/30">
            <button
              onClick={analyzeLoop}
              disabled={!selectedLoop || isAnalyzing}
              className="w-full flex items-center justify-center gap-2 px-4 py-2 bg-primary text-primary-foreground rounded-md hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
            >
              {isAnalyzing ? (
                <>
                  <Loader2 className="w-4 h-4 animate-spin" />
                  Analyzing...
                </>
              ) : (
                <>
                  <Play className="w-4 h-4" />
                  Analyze Selected Loop
                </>
              )}
            </button>
          </div>

          <div className="flex-1 overflow-y-auto">
            {loops.length === 0 ? (
              <div className="p-4 text-center text-muted-foreground">
                <Sparkles className="w-8 h-8 mx-auto mb-2 opacity-50" />
                <p className="text-sm">No AI workflow loops yet</p>
                <p className="text-xs mt-1">Run an AI workflow to generate loops for analysis</p>
              </div>
            ) : (
              <div className="divide-y divide-border">
                {loops.map((loop) => (
                  <button
                    key={loop.id}
                    onClick={() => setSelectedLoopId(loop.id)}
                    className={`w-full p-3 text-left hover:bg-muted/50 transition-colors ${
                      selectedLoopId === loop.id ? "bg-primary/10 border-l-2 border-primary" : ""
                    }`}
                  >
                    <div className="flex items-center justify-between">
                      <span className="font-medium text-sm">{loop.label}</span>
                      {loop.isActive && (
                        <span className="text-xs bg-green-500/20 text-green-600 px-1.5 py-0.5 rounded">
                          Active
                        </span>
                      )}
                    </div>
                    <p className="text-xs text-muted-foreground truncate mt-1">
                      {loop.promptPreview}
                    </p>
                    <div className="flex items-center gap-3 mt-2 text-xs text-muted-foreground">
                      <span className="flex items-center gap-1">
                        <Clock className="w-3 h-3" />
                        {formatTime(loop.startTime)}
                      </span>
                      <span>{getLoopDuration(loop)}</span>
                      <span>{loop.entries.length} entries</span>
                      {loop.issueCount > 0 && (
                        <span className="flex items-center gap-1 text-yellow-600">
                          <AlertCircle className="w-3 h-3" />
                          {loop.issueCount}
                        </span>
                      )}
                    </div>
                  </button>
                ))}
              </div>
            )}
          </div>
        </div>

        {/* Right Panel: Learnings */}
        <div className="flex-1 flex flex-col border border-border rounded-lg overflow-hidden">
          <div className="p-3 border-b border-border bg-muted/30">
            <h3 className="text-sm font-medium">Generated Learnings</h3>
            <p className="text-xs text-muted-foreground mt-1">
              Insights extracted from analyzed loops
            </p>
          </div>

          <div className="flex-1 overflow-y-auto">
            {learnings.length === 0 ? (
              <div className="p-8 text-center text-muted-foreground">
                <BookOpen className="w-12 h-12 mx-auto mb-3 opacity-50" />
                <p className="text-sm">No learnings generated yet</p>
                <p className="text-xs mt-1">
                  Select a loop and click "Analyze" to extract learnings
                </p>
              </div>
            ) : (
              <div className="divide-y divide-border">
                {learnings.map((learning) => (
                  <div key={learning.id} className="p-4">
                    <div className="flex items-start justify-between">
                      <div>
                        <div className="flex items-center gap-2">
                          <CheckCircle className="w-4 h-4 text-green-500" />
                          <span className="font-medium text-sm">
                            Analysis of {learning.loopLabel}
                          </span>
                        </div>
                        <p className="text-xs text-muted-foreground mt-1">
                          Generated {formatTime(learning.generatedAt)}
                        </p>
                      </div>
                      <div className="flex items-center gap-1">
                        <button
                          onClick={() => copyToClipboard(learning.content, learning.id)}
                          className="p-1.5 text-muted-foreground hover:text-foreground transition-colors"
                          title="Copy to clipboard"
                        >
                          {copySuccess === learning.id ? (
                            <CheckCircle className="w-4 h-4 text-green-500" />
                          ) : (
                            <Copy className="w-4 h-4" />
                          )}
                        </button>
                        <button
                          onClick={() =>
                            setExpandedLearningId(
                              expandedLearningId === learning.id ? null : learning.id,
                            )
                          }
                          className="p-1.5 text-muted-foreground hover:text-foreground transition-colors"
                        >
                          {expandedLearningId === learning.id ? (
                            <ChevronUp className="w-4 h-4" />
                          ) : (
                            <ChevronDown className="w-4 h-4" />
                          )}
                        </button>
                      </div>
                    </div>

                    {expandedLearningId === learning.id && (
                      <div className="mt-3 p-3 bg-muted/30 rounded-md">
                        <pre className="text-sm whitespace-pre-wrap font-mono">
                          {learning.content}
                        </pre>
                        <div className="flex items-center gap-2 mt-3 pt-3 border-t border-border">
                          <button
                            onClick={() => copyToClipboard(learning.content, `${learning.id}-full`)}
                            className="flex items-center gap-1.5 px-3 py-1.5 text-xs bg-background border border-border rounded hover:bg-muted transition-colors"
                          >
                            <Copy className="w-3 h-3" />
                            Copy
                          </button>
                          <button
                            onClick={() => {
                              // Format for CLAUDE.md
                              const claudeMdContent = `\n## Learnings from ${learning.loopLabel}\n\n${learning.content}`;
                              copyToClipboard(claudeMdContent, `${learning.id}-claude`);
                            }}
                            className="flex items-center gap-1.5 px-3 py-1.5 text-xs bg-background border border-border rounded hover:bg-muted transition-colors"
                          >
                            <FileText className="w-3 h-3" />
                            Copy for CLAUDE.md
                          </button>
                        </div>
                      </div>
                    )}
                  </div>
                ))}
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
