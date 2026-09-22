/**
 * Single owner of the terminal UI's top-right advisory corner.
 *
 * Finding c91550f0 (post-merge follow-up to #1633): `MidSessionToast`,
 * `HoldingLockBanner`, `WaitingLockBanner`, `ResumeFailedBanner` and
 * `SessionRecoveryBanner` each self-positioned at `absolute top-2 right-2
 * z-30` inside DIFFERENT positioned ancestors (`App.tsx`'s `<main>` for
 * `SessionRecoveryBanner`, `TerminalPage`'s zone container for the other
 * four) and are NOT mutually exclusive with each other — the only exclusive
 * pair is Holding vs Waiting. Two showing at once render superimposed, so
 * one hides the other's identifying text (holder names, file basenames,
 * session names) — exactly what served policy `ux-priorities`
 * `no-widget-may-hide-identifying-text` forbids.
 *
 * `AdvisoryStackProvider` mounts ONE `position: fixed` container near the
 * app root; `AdvisorySlot` portals a banner's content into it from wherever
 * that banner actually lives in the tree, so the five stay unaware of each
 * other while never sharing a corner unstacked. `fixed` rather than
 * `absolute`: the slots live in different positioned ancestors, and a
 * viewport anchor is the only one all of them agree on regardless of which
 * container they render from.
 *
 * `AdvisorySlot` degrades to an in-place render when no provider is mounted
 * above it (an isolated component test rendering a banner directly, with no
 * `AdvisoryStackProvider` in the tree) — using the stack is production
 * wiring's choice, not a requirement the component enforces on its callers.
 */

import { createContext, useContext, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { useUIElement } from "@qontinui/ui-bridge";

// `undefined` = no provider mounted above (fall back to an in-place render).
// `null` = a provider is mounted but its container node has not attached yet.
// `HTMLDivElement` = the portal target is ready.
const AdvisoryStackContext = createContext<HTMLDivElement | null | undefined>(undefined);

export function AdvisoryStackProvider({ children }: { children: ReactNode }) {
  const [node, setNode] = useState<HTMLDivElement | null>(null);

  // Registered so an automated occlusion sweep can name the STACK as the
  // occluder, not just the things beneath it — the same reasoning
  // `ZoneMinimap.tsx` records for itself.
  const { ref } = useUIElement({
    id: "terminal-advisory-stack",
    type: "generic",
    label: "Advisory banner stack",
  });

  return (
    <AdvisoryStackContext.Provider value={node}>
      {children}
      <div
        ref={(el) => {
          ref(el);
          setNode(el);
        }}
        data-ui-bridge-id="terminal.advisory-stack"
        className="fixed top-2 right-2 z-30 flex flex-col items-end gap-2 pointer-events-none"
      />
    </AdvisoryStackContext.Provider>
  );
}

export function AdvisorySlot({ children }: { children: ReactNode }) {
  const node = useContext(AdvisoryStackContext);

  if (node === undefined) {
    // No provider in the tree — e.g. a component test that renders a banner
    // directly. Render in place so existing tests keep seeing exactly the
    // DOM they already assert on.
    return <>{children}</>;
  }

  if (node === null) return null; // provider mounted, container ref not attached yet

  return createPortal(<div className="pointer-events-auto">{children}</div>, node);
}
