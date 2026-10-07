/**
 * ResourceGuardDialog — the blocking "Start anyway?" surface for a CRITICAL
 * spawn-time resource refusal (plan
 * `2026-08-07-runner-resource-guard-and-session-protection.md` §Part D step 3).
 *
 * Mounted once at the App root and prop-less: it subscribes to
 * `src/lib/resourceGuard.ts`'s queue itself, because the spawn that got refused
 * can originate anywhere (a terminal tab, a worker launch button, the Instances
 * panel) while the dialog must exist exactly once.
 *
 * Built on `components/ui/ConfirmDialog` — the runner's real blocking dialog:
 * `fixed inset-0 z-50` with a backdrop, already used by `LibraryBuilderLayout`
 * and `WorkflowBuilderTab`. Deliberately NOT `components/ConflictModal.tsx`,
 * which despite its name is a self-subscribing toast banner (`fixed bottom-4
 * right-4`, `role="alert"`, no backdrop) that kept its name only for import
 * compatibility — it cannot block anything.
 *
 * The override exists because a false positive here blocks the operator's actual
 * work, which is a worse failure than an occasional missed warning. There is no
 * silent-refusal path: either this dialog is on screen, or the refusal reached a
 * caller that reports it (coord `report_spawn_failed`, an HTTP error body).
 *
 * Every word comes from `lib/resourceGuardCopy.ts`: titled by the lane that
 * actually refused (never a hard-coded "Low memory"), the consequence named for
 * this OS only where it is true, the number of coalesced starts, the caller that
 * asked, and how far "Start anyway" reaches (the lane-scoped grant).
 */

import { ConfirmDialog } from "./ui";
import { resolvePendingResourceBlock, usePendingResourceBlock } from "@/lib/resourceGuard";
import { detectGuardOs, resourceGuardDialogCopy } from "@/lib/resourceGuardCopy";

export function ResourceGuardDialog() {
  const pending = usePendingResourceBlock();
  if (!pending) return null;
  const copy = resourceGuardDialogCopy(pending, detectGuardOs());

  return (
    <ConfirmDialog
      open
      variant="danger"
      title={copy.title}
      message={copy.message}
      description={copy.description}
      confirmText={copy.confirmText}
      cancelText={copy.cancelText}
      onConfirm={() => resolvePendingResourceBlock(true)}
      onClose={() => resolvePendingResourceBlock(false)}
    />
  );
}
