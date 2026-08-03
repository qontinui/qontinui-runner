/**
 * The in-place remedy for "this project has no address to open yet"
 * (projects-dashboard plan §7.1).
 *
 * ## Why this exists
 *
 * `Open` used to fail with *"Add one (for example http://localhost:3000) and
 * try again"* — an instruction the product could not carry out. The Rust
 * command `set_project_front_page` existed and was registered, but **no
 * frontend code ever called it**, so there was nowhere in the UI to add an
 * address. A remedy the user cannot perform is worse than no message: it reads
 * as "you forgot something" when the truth is "we never built the field".
 *
 * ## Two paths, in the order a non-developer needs them
 *
 * 1. **Set it up for me** — `autoconfigure_project` reuses the setup wizard's
 *    own framework detection to create and bind the right dev-server process
 *    and derive the address from its port. This matters most for the case that
 *    prompted it: a React Native project serves no web page at all, so the user
 *    would otherwise have had to know that Expo means `localhost:8081`. That is
 *    exactly the developer knowledge this dashboard exists to remove.
 * 2. **Type an address** — the escape hatch, always available, for a project
 *    whose framework we do not recognise or which is already deployed
 *    somewhere.
 *
 * Both are non-destructive and reversible: they fill empty fields and add a
 * process config the user can edit or delete.
 */

import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import { createLogger } from "@/lib/logger";
import type { SavedProject } from "@/hooks/useSavedProjects";

const log = createLogger("FrontPageSetup");

interface AutoconfigureOutcome {
  detectedFramework: string;
  frontPageUrl: string | null;
  processNames: string[];
}

export interface FrontPageSetupProps {
  project: SavedProject;
  /** Called after the project is updated, so the caller can refresh + retry. */
  onConfigured: (url: string) => void;
}

export function FrontPageSetup({ project, onConfigured }: FrontPageSetupProps) {
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** Set when detection ran and found nothing — so we say so once, plainly. */
  const [autoFailed, setAutoFailed] = useState(false);

  const runAuto = async () => {
    setBusy(true);
    setError(null);
    try {
      const outcome = await invoke<AutoconfigureOutcome>("autoconfigure_project", {
        id: project.id,
      });
      if (outcome.frontPageUrl) {
        onConfigured(outcome.frontPageUrl);
        return;
      }
      // Detection ran and could not answer. Say that rather than implying the
      // user did something wrong — and leave the manual field as the way out.
      setAutoFailed(true);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      log.error("autoconfigure_project failed", msg);
      setError(msg);
    } finally {
      setBusy(false);
    }
  };

  const saveManual = async () => {
    const trimmed = url.trim();
    if (!trimmed) return;
    setBusy(true);
    setError(null);
    try {
      await invoke("set_project_front_page", { id: project.id, url: trimmed });
      onConfigured(trimmed);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      log.error("set_project_front_page failed", msg);
      setError(msg);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="mt-2 space-y-2" data-testid={`front-page-setup-${project.id}`}>
      <button
        type="button"
        onClick={() => void runAuto()}
        disabled={busy}
        className="w-full rounded-md bg-primary px-3 py-1.5 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-60"
      >
        {busy ? "Setting up…" : "Set it up for me"}
      </button>

      {autoFailed ? (
        <p className="text-xs text-muted-foreground">
          We couldn&apos;t work out how this project starts. Enter the address it runs on:
        </p>
      ) : (
        <p className="text-xs text-muted-foreground">Or enter the address it runs on:</p>
      )}

      <div className="flex gap-2">
        <input
          type="text"
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") void saveManual();
          }}
          placeholder="http://localhost:3000"
          disabled={busy}
          className="input flex-1 rounded-md border border-border bg-background px-2 py-1 text-sm"
          aria-label="Front page address"
        />
        <button
          type="button"
          onClick={() => void saveManual()}
          disabled={busy || !url.trim()}
          className="rounded-md border border-border px-3 py-1 text-sm font-medium hover:bg-accent hover:text-accent-foreground disabled:opacity-60"
        >
          Save
        </button>
      </div>

      {error ? <p className="text-xs text-red-600 dark:text-red-400">{error}</p> : null}
    </div>
  );
}
