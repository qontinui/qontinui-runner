/**
 * The project's web address, as a first-class editable field (plan §5.2).
 *
 * Distinct from `FrontPageSetup`, which is the *recovery* affordance shown when
 * `Open` fails for want of an address. This one is always present in the detail
 * view, because the address is a property of the project and not merely the
 * cure for one error:
 *
 * - A project may already HAVE an address that needs changing — a staging URL
 *   that moved, a port that changed. The recovery path never appears for those,
 *   so without this there is no way to edit one once set.
 * - An address is often known BEFORE anything fails. A project whose backend is
 *   already deployed (an AWS URL, say) has nothing local to start and nothing to
 *   go wrong; the user simply wants to record where it lives. Making them
 *   trigger a failure first, to reach the field, would be a poor trade.
 * - Discoverability: `ux-priorities` ranks it directly behind predictability. A
 *   setting reachable only through an error is, for practical purposes, hidden.
 *
 * Remote addresses are fully supported — `validate_preview_url` accepts
 * `http:` and `https:` alike, so a deployed `https://…` host is as valid here as
 * `http://localhost:3000`. The scheme restriction is a security boundary
 * (`front_page_url` lands in a webview the app CSP does not govern, so `file:`
 * would make Open an arbitrary-file viewer), not a localhost-only rule.
 */

import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import { createLogger } from "@/lib/logger";
import type { SavedProject } from "@/hooks/useSavedProjects";

const log = createLogger("FrontPageAddress");

export interface FrontPageAddressProps {
  project: SavedProject;
  /** Called after a successful save/clear so the caller can re-read the registry. */
  onSaved?: (url: string | null) => void | Promise<void>;
}

/**
 * Local pre-check mirroring `validate_preview_url`'s scheme rule.
 *
 * Deliberately a MIRROR and not a replacement: the Rust side still validates,
 * because a client-side check is a courtesy and never a boundary. Its only job
 * is to say "that will not work" before a round-trip, in the same terms.
 */
export function describeAddressProblem(raw: string): string | null {
  const value = raw.trim();
  if (!value) return null; // empty means "clear it", handled by the caller
  let parsed: URL;
  try {
    parsed = new URL(value);
  } catch {
    return "That doesn't look like a full address. Include http:// or https://.";
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
    return `Only http:// and https:// addresses can be opened — '${parsed.protocol}' can't.`;
  }
  return null;
}

export function FrontPageAddress({ project, onSaved }: FrontPageAddressProps) {
  const stored = project.frontPageUrl ?? "";
  const [value, setValue] = useState(stored);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  // Re-sync when the registry refreshes underneath us, or the user switches
  // projects — otherwise the field keeps showing a stale edit after a
  // background refresh replaced the record.
  //
  // Adjusted DURING RENDER rather than in an effect. This is React's documented
  // pattern for deriving state from a changed prop: an effect would render once
  // with the stale value and then immediately re-render, which is both a
  // visible flash and what `react-hooks/set-state-in-effect` warns about.
  const [syncedFrom, setSyncedFrom] = useState({ id: project.id, stored });
  if (syncedFrom.id !== project.id || syncedFrom.stored !== stored) {
    setSyncedFrom({ id: project.id, stored });
    setValue(stored);
    setError(null);
    setSaved(false);
  }

  const dirty = value.trim() !== stored.trim();

  const save = async () => {
    const trimmed = value.trim();
    const problem = describeAddressProblem(trimmed);
    if (problem) {
      setError(problem);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      // An empty field CLEARS the address (`url: null`), which is the honest
      // inverse of setting one — a project that moved back to being purely
      // local should not be stuck advertising a dead host.
      await invoke("set_project_front_page", { id: project.id, url: trimmed || null });
      setSaved(true);
      await onSaved?.(trimmed || null);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      log.error("set_project_front_page failed", msg);
      setError(msg);
    } finally {
      setBusy(false);
    }
  };

  return (
    <section data-testid={`front-page-address-${project.id}`}>
      <h2 className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
        Web address
      </h2>
      <p className="mt-1 text-sm text-muted-foreground">
        Where this project can be seen. A local dev server, or somewhere it is already deployed.
      </p>
      <div className="mt-2 flex gap-2">
        <input
          type="text"
          value={value}
          onChange={(e) => {
            setValue(e.target.value);
            setSaved(false);
            setError(null);
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter") void save();
          }}
          placeholder="http://localhost:3000 or https://example.com"
          disabled={busy}
          className="input flex-1 rounded-md border border-border bg-background px-2 py-1 text-sm"
          aria-label="Web address"
        />
        <button
          type="button"
          onClick={() => void save()}
          disabled={busy || !dirty}
          className="rounded-md border border-border px-3 py-1 text-sm font-medium hover:bg-accent hover:text-accent-foreground disabled:opacity-60"
        >
          {busy ? "Saving…" : "Save"}
        </button>
      </div>
      {error ? <p className="mt-1 text-xs text-red-600 dark:text-red-400">{error}</p> : null}
      {saved && !dirty && !error ? (
        <p className="mt-1 text-xs text-muted-foreground">{stored ? "Saved." : "Cleared."}</p>
      ) : null}
    </section>
  );
}
