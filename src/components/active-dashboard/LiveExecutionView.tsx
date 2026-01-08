/**
 * LiveExecutionView Component
 *
 * Main view container that combines the screenshot view and action stream.
 * Takes up the left 70% of the dashboard.
 */

import { ScreenshotView } from "./ScreenshotView";
import { ActionStream } from "./ActionStream";
import type { LiveExecutionViewProps } from "./types";

export function LiveExecutionView({ actions, status, currentAction }: LiveExecutionViewProps) {
  return (
    <div className="flex w-[70%] flex-col border-r border-border">
      <ScreenshotView status={status} />
      <ActionStream actions={actions} currentAction={currentAction} />
    </div>
  );
}
