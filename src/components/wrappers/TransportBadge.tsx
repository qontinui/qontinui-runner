/**
 * TransportBadge — pill identifying the wrapper's transport kind.
 *
 * Only `api` is enabled in the runner per the locked architectural decision;
 * the other variants (`headless`, `headed`, `live`) still render with a
 * distinct color so wrappers that ship them stay legible in the UI.
 */

import type { WrapperTransport } from "@/lib/wrappers/types";

export interface TransportBadgeProps {
  transport: WrapperTransport;
  size?: "sm" | "md";
}

const STYLES: Record<WrapperTransport, { bg: string; text: string; border: string }> = {
  api: {
    bg: "bg-cyan-500/10",
    text: "text-cyan-400",
    border: "border-cyan-500/40",
  },
  headless: {
    bg: "bg-purple-500/10",
    text: "text-purple-400",
    border: "border-purple-500/40",
  },
  headed: {
    bg: "bg-blue-500/10",
    text: "text-blue-400",
    border: "border-blue-500/40",
  },
  live: {
    bg: "bg-emerald-500/10",
    text: "text-emerald-400",
    border: "border-emerald-500/40",
  },
};

export function TransportBadge({ transport, size = "sm" }: TransportBadgeProps) {
  const style = STYLES[transport] ?? STYLES.api;
  const sizing = size === "md" ? "px-2.5 py-1 text-xs" : "px-2 py-0.5 text-[11px]";
  return (
    <span
      className={`inline-flex items-center rounded-full border font-medium uppercase tracking-wider ${sizing} ${style.bg} ${style.text} ${style.border}`}
    >
      {transport}
    </span>
  );
}
