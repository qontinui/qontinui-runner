/**
 * UserInputPanel.tsx
 *
 * Panel component for displaying and answering all pending user input requests.
 * Shows questions from findings that need user decisions before AI can continue.
 */

import { useState, useMemo, useCallback, useEffect } from "react";
import {
  MessageSquare,
  CheckCircle,
  AlertCircle,
  Send,
  Play,
  FileText,
  ChevronDown,
  ChevronUp,
} from "lucide-react";
import { getAccentColors } from "@/design-system";
import type { Finding, UserInputRequest, UserInputOption } from "../../types/findings";
import { findingsTracker } from "../../services";
import { getCategoryById, getCategoryColorClasses } from "../../services/FindingCategories";

interface UserInputPanelProps {
  /** Findings that need user input (if not provided, fetches from tracker) */
  findings?: Finding[];
  /** Callback when user provides a response to a finding */
  onProvideResponse?: (findingId: string, response: string) => void;
  /** Callback when user clicks "Continue with Answers" */
  onContinue?: () => void;
  /** Whether the continue action is in progress */
  isContinuing?: boolean;
  /** Whether to show the panel even when there are no pending inputs */
  showEmptyState?: boolean;
}

interface AnswerState {
  [findingId: string]: string;
}

/**
 * Renders the appropriate input control based on input type
 */
function InputControl({
  request,
  value,
  onChange,
  onSubmit,
}: {
  request: UserInputRequest;
  value: string;
  onChange: (value: string) => void;
  onSubmit?: () => void;
}) {
  switch (request.inputType) {
    case "choice":
      return (
        <div className="space-y-2">
          {request.options?.map((option: UserInputOption) => (
            <button
              key={option.value}
              onClick={() => onChange(option.value)}
              className={`w-full text-left px-3 py-2 text-sm rounded border transition-colors ${
                value === option.value
                  ? "bg-primary/20 border-primary text-foreground"
                  : "bg-muted/50 border-border/50 hover:bg-muted"
              }`}
            >
              <div className="font-medium">{option.label}</div>
              {option.description && (
                <div className="text-xs text-muted-foreground mt-0.5">{option.description}</div>
              )}
            </button>
          ))}
        </div>
      );

    case "boolean": {
      const greenColors = getAccentColors("green");
      const redColors = getAccentColors("red");
      return (
        <div className="flex gap-2">
          <button
            onClick={() => onChange("yes")}
            className={`flex-1 px-3 py-2 text-sm rounded border transition-colors ${
              value === "yes"
                ? `bg-green-500/30 border-green-500 ${greenColors.text}`
                : `${greenColors.bg} ${greenColors.border} ${greenColors.text} hover:bg-green-500/20`
            }`}
          >
            Yes
          </button>
          <button
            onClick={() => onChange("no")}
            className={`flex-1 px-3 py-2 text-sm rounded border transition-colors ${
              value === "no"
                ? `bg-red-500/30 border-red-500 ${redColors.text}`
                : `${redColors.bg} ${redColors.border} ${redColors.text} hover:bg-red-500/20`
            }`}
          >
            No
          </button>
        </div>
      );
    }

    case "code":
      return (
        <div className="space-y-2">
          <textarea
            aria-label="Enter code"
            value={value}
            onChange={(e) => onChange(e.target.value)}
            placeholder="Enter code..."
            className="w-full px-3 py-2 text-sm bg-background border border-border rounded font-mono min-h-[100px] focus:outline-hidden focus:ring-2 focus:ring-primary resize-y"
          />
          {onSubmit && (
            <button
              onClick={onSubmit}
              disabled={!value.trim()}
              className="flex items-center gap-1 px-3 py-1.5 text-sm bg-primary text-primary-foreground rounded hover:bg-primary/90 disabled:opacity-50 transition-colors"
            >
              <Send className="w-3 h-3" />
              Submit
            </button>
          )}
        </div>
      );

    case "text":
    default:
      return (
        <div className="flex gap-2">
          <input
            type="text"
            aria-label="Enter your response"
            value={value}
            onChange={(e) => onChange(e.target.value)}
            placeholder={request.defaultValue || "Enter your response..."}
            className="flex-1 px-3 py-2 text-sm bg-background border border-border rounded focus:outline-hidden focus:ring-2 focus:ring-primary"
            onKeyDown={(e) => {
              if (e.key === "Enter" && value.trim() && onSubmit) {
                onSubmit();
              }
            }}
          />
          {onSubmit && (
            <button
              onClick={onSubmit}
              disabled={!value.trim()}
              className="flex items-center gap-1 px-3 py-1.5 text-sm bg-primary text-primary-foreground rounded hover:bg-primary/90 disabled:opacity-50 transition-colors"
            >
              <Send className="w-3 h-3" />
            </button>
          )}
        </div>
      );
  }
}

/**
 * Individual question card within the panel
 */
function QuestionCard({
  finding,
  answer,
  onAnswerChange,
  onSubmit,
  isAnswered,
}: {
  finding: Finding;
  answer: string;
  onAnswerChange: (value: string) => void;
  onSubmit: () => void;
  isAnswered: boolean;
}) {
  const [isExpanded, setIsExpanded] = useState(true);
  const question = finding.pendingQuestion;
  const category = getCategoryById(finding.categoryId);
  const categoryColors = getCategoryColorClasses(category?.color || "slate");

  if (!question) return null;

  const greenColors = getAccentColors("green");
  const purpleColors = getAccentColors("purple");

  return (
    <div
      className={`rounded-lg border transition-all ${
        isAnswered ? `${greenColors.border} bg-green-500/5` : "border-border bg-card"
      }`}
    >
      {/* Question Header */}
      <button
        onClick={() => setIsExpanded(!isExpanded)}
        className="w-full flex items-start gap-3 p-4 text-left"
      >
        {/* Status Icon */}
        <div className="shrink-0 mt-0.5">
          {isAnswered ? (
            <CheckCircle className={`w-5 h-5 ${greenColors.text}`} />
          ) : (
            <MessageSquare className={`w-5 h-5 ${purpleColors.text}`} />
          )}
        </div>

        {/* Question Content */}
        <div className="flex-1 min-w-0">
          {/* Finding Title */}
          <div className="flex items-center gap-2 flex-wrap">
            <span
              className={`text-xs px-1.5 py-0.5 rounded ${categoryColors.bg} ${categoryColors.text}`}
            >
              {category?.name || finding.categoryId}
            </span>
            <h4 className="font-medium text-sm text-foreground">{finding.title}</h4>
          </div>

          {/* Question */}
          <p className="text-sm text-foreground mt-2">
            {question.question}
            {question.required && <span className={`${getAccentColors("red").text} ml-1`}>*</span>}
          </p>

          {/* Answered Status */}
          {isAnswered && <p className={`text-xs ${greenColors.text} mt-1`}>Answered: {answer}</p>}
        </div>

        {/* Expand/Collapse */}
        <div className="shrink-0 text-muted-foreground">
          {isExpanded ? <ChevronUp className="w-4 h-4" /> : <ChevronDown className="w-4 h-4" />}
        </div>
      </button>

      {/* Expanded Content */}
      {isExpanded && (
        <div className="px-4 pb-4 space-y-3 border-t border-border/50 pt-3 ml-8">
          {/* Context */}
          {question.context && <p className="text-xs text-muted-foreground">{question.context}</p>}

          {/* File Context */}
          {finding.codeContext?.file && (
            <p className="text-xs text-muted-foreground flex items-center gap-1">
              <FileText className="w-3 h-3" />
              {finding.codeContext.file}
              {finding.codeContext.line && `:${finding.codeContext.line}`}
            </p>
          )}

          {/* Input Control */}
          <InputControl
            request={question}
            value={answer}
            onChange={onAnswerChange}
            onSubmit={onSubmit}
          />
        </div>
      )}
    </div>
  );
}

export function UserInputPanel({
  findings: propFindings,
  onProvideResponse,
  onContinue,
  isContinuing = false,
  showEmptyState = false,
}: UserInputPanelProps) {
  const [answers, setAnswers] = useState<AnswerState>({});
  const [liveFindings, setLiveFindings] = useState<Finding[]>([]);

  // Subscribe to live findings updates if not provided as prop
  useEffect(() => {
    if (propFindings) return;

    const unsubscribe = findingsTracker.subscribe((event) => {
      if (
        event.type === "finding_detected" ||
        event.type === "finding_updated" ||
        event.type === "input_received"
      ) {
        setLiveFindings(findingsTracker.getFindingsNeedingInput());
      }
    });

    // Initialize with current findings
    // eslint-disable-next-line react-hooks/set-state-in-effect -- subscription initialization
    setLiveFindings(findingsTracker.getFindingsNeedingInput());

    return unsubscribe;
  }, [propFindings]);

  // Use prop findings or live findings
  const findingsNeedingInput = useMemo(() => {
    if (propFindings) {
      return propFindings.filter((f) => f.status === "needs_input" && f.pendingQuestion);
    }
    return liveFindings;
  }, [propFindings, liveFindings]);

  // Calculate statistics
  const stats = useMemo(() => {
    const total = findingsNeedingInput.length;
    const answered = findingsNeedingInput.filter((f) => {
      const answer = answers[f.id];
      return answer && answer.trim().length > 0;
    }).length;
    const required = findingsNeedingInput.filter((f) => f.pendingQuestion?.required).length;
    const requiredAnswered = findingsNeedingInput.filter((f) => {
      if (!f.pendingQuestion?.required) return true;
      const answer = answers[f.id];
      return answer && answer.trim().length > 0;
    }).length;

    return {
      total,
      answered,
      required,
      requiredAnswered,
      allRequiredAnswered: requiredAnswered === total,
    };
  }, [findingsNeedingInput, answers]);

  // Handle answer change
  const handleAnswerChange = useCallback((findingId: string, value: string) => {
    setAnswers((prev) => ({ ...prev, [findingId]: value }));
  }, []);

  // Handle submitting a single answer
  const handleSubmitAnswer = useCallback(
    (findingId: string) => {
      const answer = answers[findingId];
      if (!answer?.trim()) return;

      if (onProvideResponse) {
        onProvideResponse(findingId, answer);
      } else {
        // Default: update the finding in tracker
        findingsTracker.provideUserResponse(findingId, answer);
      }
    },
    [answers, onProvideResponse],
  );

  // Handle continue with all answers
  const handleContinue = useCallback(() => {
    // First, submit all answers to the tracker
    for (const finding of findingsNeedingInput) {
      const answer = answers[finding.id];
      if (answer?.trim()) {
        if (onProvideResponse) {
          onProvideResponse(finding.id, answer);
        } else {
          findingsTracker.provideUserResponse(finding.id, answer);
        }
      }
    }

    // Then trigger continuation
    if (onContinue) {
      onContinue();
    }
  }, [findingsNeedingInput, answers, onProvideResponse, onContinue]);

  // Empty state
  if (findingsNeedingInput.length === 0) {
    if (!showEmptyState) return null;

    return (
      <div className="flex flex-col items-center justify-center p-8 text-muted-foreground">
        <CheckCircle className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-medium">No Questions Pending</p>
        <p className="text-sm mt-2 text-center">
          All findings have been answered or don't require user input.
        </p>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="shrink-0 border-b border-border p-4 space-y-3">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            <div className={`p-2 ${getAccentColors("purple").bg} rounded-lg`}>
              <MessageSquare className={`w-5 h-5 ${getAccentColors("purple").text}`} />
            </div>
            <div>
              <h2 className="font-semibold text-foreground">Questions for You</h2>
              <p className="text-xs text-muted-foreground">
                Answer these questions to help the AI continue
              </p>
            </div>
          </div>

          {/* Progress Badge */}
          <div className="flex items-center gap-2">
            <span
              className={`px-3 py-1.5 rounded-lg text-sm font-medium ${
                stats.allRequiredAnswered
                  ? `${getAccentColors("green").bg} ${getAccentColors("green").text}`
                  : `${getAccentColors("amber").bg} ${getAccentColors("amber").text}`
              }`}
            >
              {stats.answered} / {stats.total} answered
            </span>
          </div>
        </div>

        {/* Continue Button */}
        {onContinue && (
          <div className="flex justify-end">
            <button
              onClick={handleContinue}
              disabled={!stats.allRequiredAnswered || isContinuing}
              className="flex items-center gap-2 px-4 py-2 bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
            >
              {isContinuing ? (
                <>
                  <div className="w-4 h-4 border-2 border-primary-foreground/30 border-t-primary-foreground rounded-full animate-spin" />
                  Continuing...
                </>
              ) : (
                <>
                  <Play className="w-4 h-4" />
                  Continue with Answers
                </>
              )}
            </button>
          </div>
        )}

        {/* Validation Message */}
        {!stats.allRequiredAnswered && (
          <div className={`flex items-center gap-2 text-xs ${getAccentColors("amber").text}`}>
            <AlertCircle className="w-4 h-4" />
            Please answer all required questions before continuing
          </div>
        )}
      </div>

      {/* Questions List */}
      <div className="flex-1 min-h-0 overflow-y-auto p-4 space-y-3">
        {findingsNeedingInput.map((finding) => (
          <QuestionCard
            key={finding.id}
            finding={finding}
            answer={answers[finding.id] || ""}
            onAnswerChange={(value) => handleAnswerChange(finding.id, value)}
            onSubmit={() => handleSubmitAnswer(finding.id)}
            isAnswered={Boolean(answers[finding.id]?.trim())}
          />
        ))}
      </div>
    </div>
  );
}
