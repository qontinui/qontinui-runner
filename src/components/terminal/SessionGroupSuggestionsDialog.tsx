/**
 * "Suggest groups" preview — plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 6. Lists the deterministic suggestions `suggestGroups` produced, each
 * with its proposed name, color chip, member titles and the key it clustered
 * on, and offers Accept / Dismiss per suggestion plus Accept all.
 *
 * Presentational: it computes nothing and fetches nothing. Every choice is
 * handed to `onApply` as the exact org-op batch to send (`acceptOps`,
 * `dismissOp`, `acceptAllOps`), so one click is one `session_org_apply`.
 *
 * Addressability follows `SESSION_MANAGER_HEADER_IDS`: every button registers
 * under a stable `terminal.session-group-suggestions-*` id, and the dialog
 * registers a `session-group-suggestions` component whose actions declare
 * their effect class, so an agent can accept or dismiss by suggestion id
 * without locating a button.
 */

import { X } from "lucide-react";
import { UIBridgeComponentScope, useUIComponent, useUIElement } from "@qontinui/ui-bridge";

import { guardedHandler } from "@/lib/ui-bridge/guardedHandler";

import { textArg } from "./commands";
import { SESSION_GROUP_COLOR_VAR } from "./sessionGroupColors";
import {
  acceptAllOps,
  dismissOp,
  type GroupSuggestion,
  type SuggestionKeyKind,
} from "./sessionGroupSuggestions";
import type { SessionOrgOp } from "./sessionOrgTypes";

/** Author-controlled UI-Bridge ids for this dialog. */
export const SESSION_GROUP_SUGGESTIONS_IDS = {
  root: "terminal.session-group-suggestions",
  acceptAll: "terminal.session-group-suggestions-accept-all",
  close: "terminal.session-group-suggestions-close",
} as const;

/** The component id the dialog's actions register under. */
export const SESSION_GROUP_SUGGESTIONS_COMPONENT_ID = "session-group-suggestions";

/** Per-suggestion Accept button id. */
export function sessionGroupSuggestionAcceptId(suggestionId: string): string {
  return `terminal.session-group-suggestion-accept-${suggestionId}`;
}

/** Per-suggestion Dismiss button id. */
export function sessionGroupSuggestionDismissId(suggestionId: string): string {
  return `terminal.session-group-suggestion-dismiss-${suggestionId}`;
}

/** The word naming the key a suggestion clustered on. */
export const SUGGESTION_KEY_LABELS: Readonly<Record<SuggestionKeyKind, string>> = {
  plan: "plan",
  pr: "pull request",
  branch: "branch",
  repo: "repo",
};

const SUGGESTION_ID_SCHEMA = { id: "string" } as const;

interface SessionGroupSuggestionsDialogProps {
  open: boolean;
  suggestions: readonly GroupSuggestion[];
  /** One batch per click — accept, dismiss, or accept all. */
  onApply: (ops: SessionOrgOp[]) => void;
  onClose: () => void;
}

export function SessionGroupSuggestionsDialog(props: SessionGroupSuggestionsDialogProps) {
  if (!props.open) return null;
  return <DialogBody {...props} />;
}

function DialogBody({ suggestions, onApply, onClose }: SessionGroupSuggestionsDialogProps) {
  const { ref: rootRef } = useUIElement({
    id: SESSION_GROUP_SUGGESTIONS_IDS.root,
    type: "generic",
    label: "Suggested session groups",
  });
  const { ref: acceptAllRef } = useUIElement({
    id: SESSION_GROUP_SUGGESTIONS_IDS.acceptAll,
    type: "button",
    label: "Accept all suggested groups",
  });
  const { ref: closeRef } = useUIElement({
    id: SESSION_GROUP_SUGGESTIONS_IDS.close,
    type: "button",
    label: "Close suggested groups",
  });

  const byId = (id: string): GroupSuggestion => {
    const found = suggestions.find((s) => s.id === id);
    if (!found) {
      throw new Error(
        `No suggestion "${id}". Available: ${suggestions.map((s) => s.id).join(", ") || "none"}`,
      );
    }
    return found;
  };

  useUIComponent({
    id: SESSION_GROUP_SUGGESTIONS_COMPONENT_ID,
    name: "Suggested session groups",
    description:
      "Deterministic group suggestions for ungrouped sessions, clustered by plan, PR, " +
      "repo+branch or repo. Accept creates the group with its members; dismiss is remembered.",
    actions: [
      {
        id: "list-suggestions",
        label: "List Suggestions",
        // `read` — returns the suggestions on screen. A query.
        effect: "read",
        handler: () =>
          suggestions.map((s) => ({
            id: s.id,
            name: s.name,
            keyKind: s.keyKind,
            keyValue: s.keyValue,
            colorRole: s.colorRole,
            members: s.members.map((m) => m.sessionKey),
          })),
      },
      {
        id: "accept-suggestion",
        label: "Accept Suggestion",
        description: "Create the suggested group, with its members, by suggestion id.",
        paramSchema: SUGGESTION_ID_SCHEMA,
        // `write` — creates one group and files ungrouped sessions into it.
        // Reversible through the group's own ungroup / delete.
        effect: "write",
        handler: guardedHandler("accept-suggestion", SUGGESTION_ID_SCHEMA, (args) => {
          onApply(byId(textArg(args, "id")).acceptOps);
        }),
      },
      {
        id: "dismiss-suggestion",
        label: "Dismiss Suggestion",
        description: "Stop proposing this suggestion; the dismissal is stored in the org.",
        paramSchema: SUGGESTION_ID_SCHEMA,
        // `write` — records the suggestion id as dismissed. No session or
        // group changes.
        effect: "write",
        handler: guardedHandler("dismiss-suggestion", SUGGESTION_ID_SCHEMA, (args) => {
          onApply([dismissOp(byId(textArg(args, "id")))]);
        }),
      },
      {
        id: "accept-all",
        label: "Accept All Suggestions",
        // `write` — creates every suggested group in one batch; each is
        // reversible through its own ungroup / delete.
        effect: "write",
        handler: () => {
          if (suggestions.length > 0) onApply(acceptAllOps(suggestions));
        },
      },
    ],
  });

  return (
    <UIBridgeComponentScope componentId={SESSION_GROUP_SUGGESTIONS_COMPONENT_ID}>
      <div className="fixed inset-0 z-50 flex items-center justify-center">
        <div
          className="absolute inset-0 bg-black/50"
          onClick={onClose}
          role="button"
          tabIndex={-1}
          aria-label="Close suggested groups"
        />
        <div
          ref={rootRef}
          data-ui-bridge-id={SESSION_GROUP_SUGGESTIONS_IDS.root}
          data-suggestion-count={suggestions.length}
          role="dialog"
          aria-label="Suggested session groups"
          onKeyDown={(e) => {
            if (e.key === "Escape") onClose();
          }}
          className="relative w-full max-w-md mx-4 rounded-lg border border-[#2a2d3d] bg-[#13141f] shadow-xl overflow-hidden"
        >
          <div className="flex items-center justify-between px-3 py-2 border-b border-[#2a2d3d]">
            <span className="text-xs font-semibold text-[#c0caf5]">Suggested groups</span>
            <div className="flex items-center gap-1">
              <button
                ref={acceptAllRef}
                data-ui-bridge-id={SESSION_GROUP_SUGGESTIONS_IDS.acceptAll}
                disabled={suggestions.length === 0}
                onClick={() => onApply(acceptAllOps(suggestions))}
                className="px-2 py-0.5 rounded text-[10px] bg-brand-primary/15 text-brand-primary hover:bg-brand-primary/25 disabled:opacity-40"
              >
                Accept all
              </button>
              <button
                ref={closeRef}
                data-ui-bridge-id={SESSION_GROUP_SUGGESTIONS_IDS.close}
                onClick={onClose}
                aria-label="Close suggested groups"
                className="p-1 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d]"
              >
                <X className="w-3 h-3" />
              </button>
            </div>
          </div>
          {suggestions.length === 0 ? (
            <p className="px-3 py-4 text-[11px] text-[#565f89]">
              No two ungrouped sessions share a plan, pull request, branch or repo.
            </p>
          ) : (
            <ul className="max-h-[60vh] overflow-y-auto divide-y divide-[#2a2d3d]">
              {suggestions.map((s) => (
                <SuggestionRow key={s.id} suggestion={s} onApply={onApply} />
              ))}
            </ul>
          )}
        </div>
      </div>
    </UIBridgeComponentScope>
  );
}

/**
 * One suggestion. Its own component so each row's `useUIElement` calls are
 * plain unconditional hooks — same shape as the header's filter chips.
 */
function SuggestionRow({
  suggestion,
  onApply,
}: {
  suggestion: GroupSuggestion;
  onApply: (ops: SessionOrgOp[]) => void;
}) {
  const acceptId = sessionGroupSuggestionAcceptId(suggestion.id);
  const dismissId = sessionGroupSuggestionDismissId(suggestion.id);
  const { ref: acceptRef } = useUIElement({
    id: acceptId,
    type: "button",
    label: `Accept suggested group ${suggestion.name}`,
  });
  const { ref: dismissRef } = useUIElement({
    id: dismissId,
    type: "button",
    label: `Dismiss suggested group ${suggestion.name}`,
  });
  const keyLabel = SUGGESTION_KEY_LABELS[suggestion.keyKind];

  return (
    <li
      data-suggestion-id={suggestion.id}
      data-key-kind={suggestion.keyKind}
      data-color-role={suggestion.colorRole}
      className="px-3 py-2"
    >
      <div className="flex items-center gap-2">
        <span
          aria-hidden
          className="w-2.5 h-2.5 rounded-sm shrink-0"
          style={{ backgroundColor: SESSION_GROUP_COLOR_VAR[suggestion.colorRole] }}
        />
        <span
          className="text-[11px] font-medium text-[#c0caf5] truncate"
          title={suggestion.keyValue}
        >
          {suggestion.name}
        </span>
        <span className="text-[10px] text-[#565f89] shrink-0">
          {suggestion.members.length} sessions
        </span>
        <div className="ml-auto flex items-center gap-1 shrink-0">
          <button
            ref={acceptRef}
            data-ui-bridge-id={acceptId}
            onClick={() => onApply(suggestion.acceptOps)}
            className="px-1.5 py-0.5 rounded text-[10px] bg-brand-primary/15 text-brand-primary hover:bg-brand-primary/25"
          >
            Accept
          </button>
          <button
            ref={dismissRef}
            data-ui-bridge-id={dismissId}
            onClick={() => onApply([dismissOp(suggestion)])}
            className="px-1.5 py-0.5 rounded text-[10px] text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]"
          >
            Dismiss
          </button>
        </div>
      </div>
      <div className="mt-0.5 text-[10px] text-[#565f89] truncate" title={suggestion.keyValue}>
        by {keyLabel}: {suggestion.keyValue}
      </div>
      <ul className="mt-1 pl-4 list-disc text-[10px] text-[#a9b1d6]">
        {suggestion.members.map((m) => (
          <li key={m.sessionKey} className="truncate" title={m.title}>
            {m.title}
          </li>
        ))}
      </ul>
    </li>
  );
}
