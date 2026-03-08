import { useEffect } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";

export function useWindowTitle(
  needsInputCount: number,
  errorCount: number,
  isMultiZone: boolean,
): void {
  useEffect(() => {
    const actionCount = needsInputCount + errorCount;
    if (isMultiZone && actionCount > 0) {
      getCurrentWindow()
        .setTitle(`(${actionCount} waiting) Terminal - Qontinui Runner`)
        .catch(() => {});
    } else {
      getCurrentWindow()
        .setTitle("Qontinui Runner")
        .catch(() => {});
    }
    return () => {
      getCurrentWindow()
        .setTitle("Qontinui Runner")
        .catch(() => {});
    };
  }, [needsInputCount, errorCount, isMultiZone]);
}
