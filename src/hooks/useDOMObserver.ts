/**
 * useDOMObserver Hook
 *
 * Watches for DOM changes using MutationObserver and IntersectionObserver.
 * Used by the tutorial system to detect when elements appear, change, or become visible.
 */

import { useEffect, useRef, useState, useCallback } from "react";

interface DOMObserverConfig {
  /** CSS selector or data-tutorial-id to watch for */
  selector: string;
  /** Type of observation to perform */
  type: "appear" | "disappear" | "change" | "visible";
  /** Whether the observer is currently active */
  enabled: boolean;
  /** Callback when condition is met */
  onMatch?: (element: HTMLElement) => void;
  /** Additional validation function */
  validate?: (element: HTMLElement) => boolean;
}

interface UseDOMObserverReturn {
  /** Whether the condition has been matched */
  matched: boolean;
  /** The matched element (if any) */
  element: HTMLElement | null;
  /** Reset the observer state */
  reset: () => void;
}

/**
 * Resolves a selector to find a DOM element
 * Supports both data-tutorial-id attributes and CSS selectors
 */
function resolveElement(selector: string): HTMLElement | null {
  // Try data-tutorial-id first
  let element = document.querySelector(`[data-tutorial-id="${selector}"]`) as HTMLElement | null;

  // Fall back to CSS selector
  if (!element) {
    element = document.querySelector(selector) as HTMLElement | null;
  }

  return element;
}

/**
 * Hook to observe DOM changes for a specific element
 */
export function useDOMObserver({
  selector,
  type,
  enabled,
  onMatch,
  validate,
}: DOMObserverConfig): UseDOMObserverReturn {
  const [matched, setMatched] = useState(false);
  const [element, setElement] = useState<HTMLElement | null>(null);
  const mutationObserverRef = useRef<MutationObserver | null>(null);
  const intersectionObserverRef = useRef<IntersectionObserver | null>(null);

  const handleMatch = useCallback(
    (el: HTMLElement) => {
      // Apply validation if provided
      if (validate && !validate(el)) {
        return;
      }

      setMatched(true);
      setElement(el);
      onMatch?.(el);
    },
    [onMatch, validate],
  );

  const reset = useCallback(() => {
    setMatched(false);
    setElement(null);
  }, []);

  // Clean up observers
  const cleanup = useCallback(() => {
    if (mutationObserverRef.current) {
      mutationObserverRef.current.disconnect();
      mutationObserverRef.current = null;
    }
    if (intersectionObserverRef.current) {
      intersectionObserverRef.current.disconnect();
      intersectionObserverRef.current = null;
    }
  }, []);

  useEffect(() => {
    if (!enabled) {
      cleanup();
      return;
    }

    // Check if element already exists (for "appear" type)
    const existingElement = resolveElement(selector);

    switch (type) {
      case "appear": {
        if (existingElement) {
          handleMatch(existingElement);
          return;
        }

        // Set up MutationObserver to watch for element appearance
        mutationObserverRef.current = new MutationObserver(() => {
          const el = resolveElement(selector);
          if (el) {
            handleMatch(el);
            cleanup();
          }
        });

        mutationObserverRef.current.observe(document.body, {
          childList: true,
          subtree: true,
        });
        break;
      }

      case "disappear": {
        if (!existingElement) {
          setMatched(true);
          setElement(null);
          onMatch?.(null as unknown as HTMLElement);
          return;
        }

        // Watch for element removal
        mutationObserverRef.current = new MutationObserver(() => {
          const el = resolveElement(selector);
          if (!el) {
            setMatched(true);
            setElement(null);
            onMatch?.(null as unknown as HTMLElement);
            cleanup();
          }
        });

        mutationObserverRef.current.observe(document.body, {
          childList: true,
          subtree: true,
        });
        break;
      }

      case "change": {
        if (!existingElement) {
          console.warn(`[useDOMObserver] Element not found for change observation: ${selector}`);
          return;
        }

        // Watch for attribute or content changes
        mutationObserverRef.current = new MutationObserver((mutations) => {
          for (const mutation of mutations) {
            if (
              mutation.type === "characterData" ||
              mutation.type === "attributes" ||
              mutation.type === "childList"
            ) {
              handleMatch(existingElement);
              return;
            }
          }
        });

        mutationObserverRef.current.observe(existingElement, {
          attributes: true,
          characterData: true,
          childList: true,
          subtree: true,
        });
        break;
      }

      case "visible": {
        if (!existingElement) {
          // Wait for element to appear first, then check visibility
          mutationObserverRef.current = new MutationObserver(() => {
            const el = resolveElement(selector);
            if (el) {
              mutationObserverRef.current?.disconnect();
              setupIntersectionObserver(el);
            }
          });

          mutationObserverRef.current.observe(document.body, {
            childList: true,
            subtree: true,
          });
          return;
        }

        setupIntersectionObserver(existingElement);
        break;
      }
    }

    function setupIntersectionObserver(el: HTMLElement) {
      intersectionObserverRef.current = new IntersectionObserver(
        (entries) => {
          for (const entry of entries) {
            if (entry.isIntersecting) {
              handleMatch(el);
              cleanup();
              return;
            }
          }
        },
        {
          root: null, // viewport
          threshold: 0.1, // At least 10% visible
        },
      );

      intersectionObserverRef.current.observe(el);
    }

    return cleanup;
  }, [selector, type, enabled, handleMatch, cleanup, onMatch]);

  return {
    matched,
    element,
    reset,
  };
}

/**
 * Hook to watch for multiple elements at once
 */
export function useDOMObserverMultiple(configs: DOMObserverConfig[]): {
  results: Map<string, UseDOMObserverReturn>;
  allMatched: boolean;
  anyMatched: boolean;
  resetAll: () => void;
} {
  const [results, setResults] = useState<Map<string, UseDOMObserverReturn>>(new Map());

  // Create observers for each config
  useEffect(() => {
    const observerResults = new Map<string, UseDOMObserverReturn>();

    configs.forEach((config) => {
      // Initialize with default state
      observerResults.set(config.selector, {
        matched: false,
        element: null,
        reset: () => {},
      });
    });

    setResults(observerResults);
  }, [configs]);

  const allMatched = Array.from(results.values()).every((r) => r.matched);
  const anyMatched = Array.from(results.values()).some((r) => r.matched);

  const resetAll = useCallback(() => {
    results.forEach((result) => result.reset());
  }, [results]);

  return {
    results,
    allMatched,
    anyMatched,
    resetAll,
  };
}

/**
 * Simple hook to check if an element exists in the DOM
 */
export function useElementExists(selector: string, enabled: boolean = true): boolean {
  const { matched } = useDOMObserver({
    selector,
    type: "appear",
    enabled,
  });

  return matched;
}

/**
 * Hook to check if an element is visible in the viewport
 */
export function useElementVisible(selector: string, enabled: boolean = true): boolean {
  const { matched } = useDOMObserver({
    selector,
    type: "visible",
    enabled,
  });

  return matched;
}
