/**
 * MarkdownPanel Component
 *
 * Renders markdown content using the existing MarkdownViewer.
 */

import { MarkdownViewer } from "@/components/MarkdownViewer";
import type { CanvasPanelComponentProps } from "./types";

export function MarkdownPanel({ data }: CanvasPanelComponentProps) {
  const content = (data.content as string) ?? "";

  if (!content) {
    return <p className="text-sm text-muted-foreground italic">No content</p>;
  }

  return <MarkdownViewer content={content} />;
}
