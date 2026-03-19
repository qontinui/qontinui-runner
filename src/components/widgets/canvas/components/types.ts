/**
 * Props that all canvas panel components receive.
 */
export interface CanvasPanelComponentProps {
  data: Record<string, unknown>;
  size?: "compact" | "normal" | "large";
  panelId?: string;
  readOnly?: boolean;
}
