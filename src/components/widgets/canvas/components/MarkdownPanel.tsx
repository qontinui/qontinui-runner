/**
 * MarkdownPanel Component
 *
 * Renders markdown content using the existing MarkdownViewer.
 */

import { MarkdownViewer } from "@/components/MarkdownViewer";
import type { CanvasPanelComponentProps } from "./types";

export function MarkdownPanel({ data }: CanvasPanelComponentProps) {
  // Accept either `markdown` (Rust-side canonical field, e.g. from
  // canvas_panels/mod.rs::emit_panel) or `content` (legacy key). Either
  // maps to the MarkdownViewer's `content` prop.
  const content =
    (typeof data.markdown === "string" && data.markdown) ||
    (typeof data.content === "string" && data.content) ||
    "";

  if (!content) {
    return <p className="text-sm text-muted-foreground italic">No content</p>;
  }

  return <MarkdownViewer content={content} />;
}
