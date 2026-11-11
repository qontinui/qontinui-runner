/**
 * A ref that always holds the value from the latest committed render.
 *
 * Effect signatures need it. `useUIComponent` keeps the `signature` object an
 * action carried when the component registered, and does not replace it on a
 * later render. `predict` and verification both read that stored copy. So a
 * signature must read render-time state through a ref when `predicts` runs.
 * A value captured into the signature at render would stay fixed at its
 * mount-time value (no profiles loaded, no tabs reconnected, no zone
 * maximized). The ref object is stable, so the first registration's closure
 * stays correct for the component's lifetime.
 */

import { useLayoutEffect, useRef, type MutableRefObject } from "react";

export function useLiveRef<T>(value: T): MutableRefObject<T> {
  const ref = useRef(value);
  useLayoutEffect(() => {
    ref.current = value;
  });
  return ref;
}
