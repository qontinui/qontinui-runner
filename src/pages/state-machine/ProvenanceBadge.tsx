/**
 * ProvenanceBadge
 *
 * Small pill rendered next to a state name in the State Machine page,
 * surfacing the compiled provenance ("observed" / "ai-generated" /
 * "ai-fallback" / "git-supervised") plus lightweight metadata
 * (observation count, age of last observation, invalidation age,
 * git proposal kind + age).
 *
 * The `provenance` and `provenanceMeta` fields are emitted by
 * `src/lib/compile-state-machine.ts`. A provenance of `undefined` (older
 * compiled SMs that predate the pipeline change) is rendered as
 * `ai-generated` silently — the whole point of the badge is the at-a-glance
 * lifecycle signal, not a noisy "unknown". An unknown future value is
 * treated the same way (forward-compat).
 */

import type { StateProvenance, StateProvenanceMeta } from "@/lib/compile-state-machine";

export interface ProvenanceBadgeProps {
  provenance?: StateProvenance;
  provenanceMeta?: StateProvenanceMeta;
  /** Compact variant — drops the secondary text segment. */
  compact?: boolean;
  className?: string;
}

/** Human-readable duration for a timestamp in the past. */
function formatAge(iso: string | undefined): string | null {
  if (!iso) return null;
  const ms = Date.parse(iso);
  if (Number.isNaN(ms)) return null;
  const diffMs = Date.now() - ms;
  if (diffMs < 0) return "just now";
  const mins = Math.floor(diffMs / 60_000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return `${hrs}h ago`;
  const days = Math.floor(hrs / 24);
  return `${days}d ago`;
}

export function ProvenanceBadge({
  provenance,
  provenanceMeta,
  compact,
  className,
}: ProvenanceBadgeProps) {
  // Older compiled SMs didn't carry provenance — treat as ai-generated.
  const effective: StateProvenance = provenance ?? "ai-generated";

  const base =
    "inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-medium border whitespace-nowrap";

  if (effective === "observed") {
    const count = provenanceMeta?.observationCount;
    const age = formatAge(provenanceMeta?.lastObserved);
    const detail = [count != null ? `${count} obs` : null, age].filter(Boolean).join(" · ");
    return (
      <span
        className={`${base} bg-green-500/10 border-green-500/40 text-green-400 ${className ?? ""}`}
        title={
          provenanceMeta
            ? `support=${provenanceMeta.support?.toFixed(2) ?? "?"}, contrast=${provenanceMeta.contrast?.toFixed(2) ?? "?"}`
            : undefined
        }
      >
        observed{!compact && detail ? ` · ${detail}` : ""}
      </span>
    );
  }

  if (effective === "ai-fallback") {
    const age = formatAge(provenanceMeta?.invalidatedAt);
    return (
      <span
        className={`${base} bg-yellow-500/10 border-yellow-500/40 text-yellow-400 ${className ?? ""}`}
        title={
          provenanceMeta?.invalidatedAt
            ? `invalidated at ${provenanceMeta.invalidatedAt}`
            : "within 24h invalidation window"
        }
      >
        ai-fallback
        {!compact ? ` · invalidated ${age ?? "recently"} · rebuilding` : ""}
      </span>
    );
  }

  if (effective === "git-supervised") {
    // Cyan — distinct from `observed` (green) at a glance while still
    // signalling positive provenance (the state is reproducible from VCS).
    const kind = provenanceMeta?.gitProposalKind;
    const age = formatAge(provenanceMeta?.gitProposedAt);
    const detail = [kind ?? null, age].filter(Boolean).join(" · ");
    return (
      <span
        className={`${base} bg-cyan-500/10 border-cyan-500/40 text-cyan-400 ${className ?? ""}`}
        title={
          provenanceMeta?.gitProposedAt
            ? `git-supervised: ${kind ?? "event"} at ${provenanceMeta.gitProposedAt}`
            : "git-supervised: proposed by git supervision channel"
        }
      >
        git-supervised{!compact && detail ? ` · ${detail}` : ""}
      </span>
    );
  }

  // ai-generated (or any unknown forward-compat value) — gray. Omit the
  // N/M promotion-threshold segment: the compiled TS side doesn't have
  // the M threshold locally (promotion logic is threshold-based on
  // support/contrast, not observation count).
  return (
    <span
      className={`${base} bg-gray-500/10 border-gray-500/40 text-gray-400 ${className ?? ""}`}
      title="AI-generated elements; no discovery artifact match yet"
    >
      ai-generated
    </span>
  );
}
