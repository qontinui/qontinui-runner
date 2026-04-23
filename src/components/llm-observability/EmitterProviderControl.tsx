/**
 * EmitterProviderControl
 *
 * Settings widget at the top of the ScriptedOutputPanel that lets the user
 * switch which provider serves scripted-output emit calls (Claude warm,
 * legacy Claude API, or the local-Gemma lane) and configure the local
 * endpoint. Reads the current settings from
 * `get_scripted_output_settings` and persists changes via
 * `save_scripted_output_settings` — both Tauri commands are on the UI
 * Bridge invoke allowlist so this panel can be driven from tests.
 *
 * Settings are picked up by the next emit call without a runner restart
 * (the emitter re-reads from disk every call), so a successful save is
 * effectively instant.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useUIElement } from "@qontinui/ui-bridge";
import { CheckCircle2, CircleAlert, Loader2, Sliders } from "lucide-react";

type ProviderMode = "auto" | "claude_api_warm" | "claude_api" | "gemma_local_warm";

interface ScriptedOutputSettings {
  enabled: boolean;
  model: string | null;
  provider: ProviderMode;
  gemma_local_endpoint: string;
  gemma_local_model_alias: string;
}

interface ProviderOption {
  value: ProviderMode;
  label: string;
  blurb: string;
}

const PROVIDER_OPTIONS: ProviderOption[] = [
  {
    value: "auto",
    label: "Auto (Claude → Gemma local)",
    blurb:
      "Cascade: tries Claude API (key first, OAuth fallback); falls through to local Gemma if no Claude credential is available.",
  },
  {
    value: "claude_api_warm",
    label: "Force Claude (warm)",
    blurb:
      "Forced warm provider (keychain API key → OAuth fallback). Surfaces a loud error if neither credential is present.",
  },
  {
    value: "claude_api",
    label: "Force Claude (legacy API key)",
    blurb:
      "Forced keychain-API-key path. No prompt caching, no OAuth fallback. Useful for production runners with a dedicated billing key.",
  },
  {
    value: "gemma_local_warm",
    label: "Force local Gemma",
    blurb:
      "Forced local llama.cpp endpoint. Zero Anthropic traffic. Requires the gemma-server docker container to be running at the configured endpoint.",
  },
];

type ReachState = "unknown" | "probing" | "reachable" | "unreachable";

/** Hit `{endpoint}/health` with a short timeout. Used for the live badge. */
async function probeGemmaHealth(endpoint: string): Promise<boolean> {
  if (!endpoint) return false;
  try {
    const url = new URL("/health", endpoint).toString();
    const controller = new AbortController();
    const t = setTimeout(() => controller.abort(), 1500);
    const resp = await fetch(url, { signal: controller.signal });
    clearTimeout(t);
    return resp.ok;
  } catch {
    return false;
  }
}

export function EmitterProviderControl() {
  const { ref: rootRef } = useUIElement({
    id: "emitter-provider-control",
    label: "Emitter provider control",
    type: "generic",
  });
  const { ref: providerSelectRef } = useUIElement({
    id: "emitter-provider-select",
    label: "Emitter provider selector",
    type: "select",
  });
  const { ref: gemmaEndpointInputRef } = useUIElement({
    id: "emitter-gemma-endpoint",
    label: "Gemma local endpoint",
    type: "input",
  });
  const { ref: gemmaAliasInputRef } = useUIElement({
    id: "emitter-gemma-model-alias",
    label: "Gemma model alias",
    type: "input",
  });
  const { ref: saveButtonRef } = useUIElement({
    id: "emitter-provider-save",
    label: "Save emitter provider settings",
    type: "button",
  });
  const { ref: reachBadgeRef } = useUIElement({
    id: "emitter-gemma-reachable",
    label: "Gemma server reachability",
    type: "generic",
  });

  const [settings, setSettings] = useState<ScriptedOutputSettings | null>(null);
  const [dirty, setDirty] = useState<ScriptedOutputSettings | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saveState, setSaveState] = useState<"idle" | "saving" | "saved" | "error">("idle");
  const [saveError, setSaveError] = useState<string | null>(null);
  const [reach, setReach] = useState<ReachState>("unknown");

  // Hydrate from backend on mount.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const s = (await invoke("get_scripted_output_settings")) as ScriptedOutputSettings;
        if (!cancelled) {
          setSettings(s);
          setDirty(s);
        }
      } catch (err) {
        if (!cancelled) setLoadError(String(err));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // Debounced reachability probe — re-runs whenever the endpoint input
  // changes, and on mount once settings hydrate. Gemma is relevant to
  // reachability under `auto` too, so probe whenever the endpoint is set.
  const probeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => {
    const endpoint = dirty?.gemma_local_endpoint ?? "";
    if (!endpoint) {
      setReach("unknown");
      return;
    }
    setReach("probing");
    if (probeTimer.current) clearTimeout(probeTimer.current);
    probeTimer.current = setTimeout(async () => {
      const ok = await probeGemmaHealth(endpoint);
      setReach(ok ? "reachable" : "unreachable");
    }, 300);
    return () => {
      if (probeTimer.current) clearTimeout(probeTimer.current);
    };
  }, [dirty?.gemma_local_endpoint]);

  const hasUnsavedChanges =
    !!settings && !!dirty && JSON.stringify(settings) !== JSON.stringify(dirty);

  const onSave = useCallback(async () => {
    if (!dirty) return;
    setSaveState("saving");
    setSaveError(null);
    try {
      const saved = (await invoke("save_scripted_output_settings", {
        settings: dirty,
      })) as ScriptedOutputSettings;
      setSettings(saved);
      setDirty(saved);
      setSaveState("saved");
      setTimeout(() => setSaveState("idle"), 1500);
    } catch (err) {
      setSaveState("error");
      setSaveError(String(err));
    }
  }, [dirty]);

  if (loadError) {
    return (
      <div
        ref={rootRef}
        className="bg-card rounded-lg border border-border p-4 mb-4 text-sm text-red-500"
      >
        Failed to load emitter settings: {loadError}
      </div>
    );
  }

  if (!dirty) {
    return (
      <div
        ref={rootRef}
        className="bg-card rounded-lg border border-border p-4 mb-4 text-sm text-muted-foreground"
      >
        Loading emitter settings…
      </div>
    );
  }

  const gemmaRelevant = dirty.provider === "auto" || dirty.provider === "gemma_local_warm";
  const currentProvider = PROVIDER_OPTIONS.find((p) => p.value === dirty.provider);

  return (
    <div ref={rootRef} className="bg-card rounded-lg border border-border p-4 mb-4">
      <div className="flex items-center gap-2 mb-3">
        <Sliders className="w-4 h-4" />
        <h3 className="font-semibold">Emitter provider</h3>
        {hasUnsavedChanges && (
          <span className="ml-auto text-xs text-amber-500">unsaved changes</span>
        )}
      </div>

      <div className="space-y-3">
        <label className="block">
          <div className="text-xs text-muted-foreground mb-1">Provider</div>
          <select
            ref={providerSelectRef}
            value={dirty.provider}
            onChange={(e) => setDirty({ ...dirty, provider: e.target.value as ProviderMode })}
            className="w-full px-3 py-2 rounded-md border border-border bg-background text-sm"
          >
            {PROVIDER_OPTIONS.map((opt) => (
              <option key={opt.value} value={opt.value}>
                {opt.label}
              </option>
            ))}
          </select>
          {currentProvider && (
            <p className="text-xs text-muted-foreground mt-1">{currentProvider.blurb}</p>
          )}
        </label>

        {gemmaRelevant && (
          <div className="rounded-md border border-border bg-muted/30 p-3 space-y-2">
            <div className="flex items-center gap-2 text-xs text-muted-foreground">
              <span>Local Gemma server</span>
              <ReachabilityBadge reachRef={reachBadgeRef} state={reach} />
            </div>
            <label className="block">
              <div className="text-xs text-muted-foreground mb-1">Endpoint</div>
              <input
                ref={gemmaEndpointInputRef}
                type="text"
                value={dirty.gemma_local_endpoint}
                onChange={(e) => setDirty({ ...dirty, gemma_local_endpoint: e.target.value })}
                placeholder="http://localhost:8200"
                spellCheck={false}
                className="w-full px-3 py-1.5 rounded-md border border-border bg-background text-sm font-mono"
              />
            </label>
            <label className="block">
              <div className="text-xs text-muted-foreground mb-1">Model alias</div>
              <input
                ref={gemmaAliasInputRef}
                type="text"
                value={dirty.gemma_local_model_alias}
                onChange={(e) => setDirty({ ...dirty, gemma_local_model_alias: e.target.value })}
                placeholder="gemma-4-26b-a4b"
                spellCheck={false}
                className="w-full px-3 py-1.5 rounded-md border border-border bg-background text-sm font-mono"
              />
              <p className="text-xs text-muted-foreground mt-1">
                For logging / telemetry only. Routing is by endpoint, not alias.
              </p>
            </label>
          </div>
        )}

        <div className="flex items-center gap-3 pt-1">
          <button
            ref={saveButtonRef}
            onClick={onSave}
            disabled={!hasUnsavedChanges || saveState === "saving"}
            className="px-4 py-1.5 rounded-md bg-primary text-primary-foreground text-sm font-medium disabled:opacity-50 disabled:cursor-not-allowed hover:bg-primary/90"
          >
            {saveState === "saving" ? "Saving…" : saveState === "saved" ? "Saved" : "Save"}
          </button>
          {saveError && <span className="text-xs text-red-500 truncate">{saveError}</span>}
          {!hasUnsavedChanges && saveState !== "saved" && settings && (
            <span className="text-xs text-muted-foreground">
              current: <span className="font-mono">{settings.provider}</span>
            </span>
          )}
        </div>
      </div>
    </div>
  );
}

function ReachabilityBadge({
  state,
  reachRef,
}: {
  state: ReachState;
  reachRef: (el: HTMLSpanElement | null) => void;
}) {
  const common = "inline-flex items-center gap-1 text-xs px-1.5 py-0.5 rounded-md";
  if (state === "probing") {
    return (
      <span ref={reachRef} data-reach="probing" className={`${common} text-muted-foreground`}>
        <Loader2 className="w-3 h-3 animate-spin" />
        probing…
      </span>
    );
  }
  if (state === "reachable") {
    return (
      <span
        ref={reachRef}
        data-reach="reachable"
        className={`${common} text-green-500 bg-green-500/10`}
      >
        <CheckCircle2 className="w-3 h-3" />
        reachable
      </span>
    );
  }
  if (state === "unreachable") {
    return (
      <span
        ref={reachRef}
        data-reach="unreachable"
        className={`${common} text-amber-500 bg-amber-500/10`}
      >
        <CircleAlert className="w-3 h-3" />
        unreachable
      </span>
    );
  }
  return (
    <span ref={reachRef} data-reach="unknown" className={`${common} text-muted-foreground`}>
      —
    </span>
  );
}
