/**
 * TutorialStepBody — the body of one tutorial step, as both tutorial tooltips
 * render it.
 *
 * Every step's `content` is written as markdown (`TutorialStep.content` says
 * so), and most steps also carry `details` and `tips`. Both tooltips used to
 * print `content` as plain text and drop the other two, so `**bold**`, code
 * fences and tables showed raw and the details and tips never reached the
 * screen (coord finding 34c8ccd6). This component is the one place a step's
 * text becomes UI.
 */

import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Lightbulb } from "lucide-react";
import { cn } from "../../lib/utils";

/** The prose classes shared by `content` and `details`. */
const MARKDOWN_CLASSES = cn(
  "text-sm text-muted-foreground leading-relaxed space-y-2",
  "[&_strong]:text-foreground [&_strong]:font-semibold",
  "[&_code]:rounded [&_code]:bg-muted [&_code]:px-1 [&_code]:py-0.5 [&_code]:text-xs",
  "[&_pre]:overflow-x-auto [&_pre]:rounded-md [&_pre]:bg-muted [&_pre]:p-2",
  "[&_pre_code]:bg-transparent [&_pre_code]:p-0",
  "[&_ul]:list-disc [&_ul]:pl-5 [&_ol]:list-decimal [&_ol]:pl-5",
  "[&_table]:w-full [&_table]:text-xs [&_th]:text-left [&_th]:font-semibold",
  "[&_th]:border-b [&_th]:border-border [&_td]:border-b [&_td]:border-border/50",
  "[&_th]:py-1 [&_td]:py-1 [&_th]:pr-2 [&_td]:pr-2",
  "[&_a]:text-primary [&_a]:underline",
);

function StepMarkdown({ text, testId }: { text: string; testId: string }) {
  return (
    <div className={MARKDOWN_CLASSES} data-testid={testId}>
      <ReactMarkdown remarkPlugins={[remarkGfm]}>{text}</ReactMarkdown>
    </div>
  );
}

interface TutorialStepBodyProps {
  /** The step's main text, as markdown. */
  content: string;
  /** Optional longer explanation, as markdown, shown under a disclosure. */
  details?: string;
  /** Optional tips, one per entry, each as inline markdown. */
  tips?: string[];
}

export function TutorialStepBody({ content, details, tips }: TutorialStepBodyProps) {
  const shownTips = (tips ?? []).filter((t) => t.trim() !== "");
  return (
    <>
      <StepMarkdown text={content} testId="tutorial-step-content" />

      {details && details.trim() !== "" && (
        <details className="group rounded-lg border border-border/50 bg-muted/20">
          <summary className="cursor-pointer select-none px-3 py-2 text-xs font-medium text-foreground">
            More details
          </summary>
          <div className="px-3 pb-3">
            <StepMarkdown text={details} testId="tutorial-step-details" />
          </div>
        </details>
      )}

      {shownTips.length > 0 && (
        <div
          className="rounded-lg border border-border/50 bg-muted/20 p-3"
          data-testid="tutorial-step-tips"
        >
          <div className="mb-1 flex items-center gap-1 text-xs font-medium text-foreground">
            <Lightbulb className="h-3.5 w-3.5" />
            Tips
          </div>
          <ul className={cn(MARKDOWN_CLASSES, "list-disc pl-5")}>
            {shownTips.map((tip, i) => (
              <li key={i}>
                <ReactMarkdown remarkPlugins={[remarkGfm]} components={{ p: "span" }}>
                  {tip}
                </ReactMarkdown>
              </li>
            ))}
          </ul>
        </div>
      )}
    </>
  );
}
