import { useState, useCallback, useEffect, useMemo, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  FolderOpen,
  Search,
  Loader2,
  Lock,
  Globe,
  GitBranch,
  CheckSquare,
  Square,
  Download,
  AlertCircle,
  ExternalLink,
} from "lucide-react";

/** GitHub App install URL for the "connect your GitHub" CTA. */
const GITHUB_APP_INSTALL_URL =
  "https://github.com/apps/qontinui-merge-orchestrator/installations/new";

/** Map a raw `github_list_repos` failure message to a specific, actionable headline.
 *  Order matters: check the most specific signals first. */
export function classifyLoadError(raw: string): string {
  if (raw.includes("401")) {
    return "Your Qontinui sign-in has expired. Re-open Settings → Account and sign in again.";
  }
  if (raw.includes("403")) {
    return "Your account isn't authorized for this workspace's GitHub connection. (Couldn't resolve your tenant.)";
  }
  if (raw.includes("Failed to reach")) {
    return "Couldn't reach Qontinui. Check your connection and retry.";
  }
  return "Couldn't load your GitHub repositories.";
}

/** The mutually-exclusive top-level render states of the picker, in precedence order. */
export type GithubPickerView = "loading" | "error" | "signed_out" | "not_connected" | "connected";

/** Single source of truth for which top-level view the picker renders.
 *  Precedence MUST match the component's early-return order: loading → error →
 *  signed_out → not_connected → connected. In particular a thrown load failure
 *  (`loadError != null`) wins over `!signedIn`/`!connected` so an auth error never
 *  collapses to the "Connect GitHub" CTA. */
export function deriveGithubPickerView(s: {
  loading: boolean;
  loadError: string | null;
  signedIn: boolean;
  connected: boolean;
}): GithubPickerView {
  if (s.loading) return "loading";
  if (s.loadError != null) return "error";
  if (!s.signedIn) return "signed_out";
  if (!s.connected) return "not_connected";
  return "connected";
}

/** Wizard-internal project shape (mirrors ProjectStep's `Project`). */
interface Project {
  path: string;
  name: string;
  type: string;
  manifest: string;
}

/** Wire shape of `github_list_repos` (Tauri command → web backend). */
interface RepoListResponse {
  signed_in: boolean;
  connected: boolean;
  repos: GhRepo[];
}

interface GhRepo {
  nameWithOwner: string;
  description: string;
  isPrivate: boolean;
  updatedAt: string;
  language: string | null;
}

interface GithubRepoPickerProps {
  /** Called with the detected projects after each repo is successfully cloned. */
  onProjectsCloned: (projects: Project[]) => void;
}

/**
 * GitHub repository picker for the setup wizard's Projects step.
 *
 * Reuses the GitHub App connection the user already made during qontinui
 * onboarding — NOT a local `gh` login or a pasted token. The `github_list_repos`
 * and `github_clone_repo` Tauri commands (see
 * `src-tauri/src/commands/setup_wizard.rs`) call the qontinui-web backend with
 * the runner's Cognito bearer; web proxies to coord, which owns the GitHub App
 * key, lists the tenant's installation repos, and mints a repo-scoped short-TTL
 * clone token. The runner never stores a long-lived GitHub credential.
 *
 * Three gate states drive the UI: not signed in → sign-in hint; signed in but
 * the App isn't installed → "connect your GitHub" CTA; connected → the picker.
 *
 * After a repo clones, we scan its root (`scan_workspace_for_setup`, depth 1) to
 * detect the project type/manifest so it flows into the rest of the wizard like
 * a locally-discovered project. If detection turns up nothing we fall back to a
 * generic entry for the repo root.
 */
export function GithubRepoPicker({ onProjectsCloned }: GithubRepoPickerProps) {
  const [signedIn, setSignedIn] = useState(true);
  const [connected, setConnected] = useState(false);
  const [repos, setRepos] = useState<GhRepo[]>([]);
  const [loading, setLoading] = useState(true);
  /** Load-time failure surface (thrown `github_list_repos` — auth/coord/network).
   *  SEPARATE from the clone-failure `error` state below, which belongs only to
   *  the connected picker view. */
  const [loadError, setLoadError] = useState<string | null>(null);

  const [filter, setFilter] = useState("");
  const [selectedRepos, setSelectedRepos] = useState<string[]>([]);
  const [destParent, setDestParent] = useState("");

  const [cloning, setCloning] = useState(false);
  const [cloneProgress, setCloneProgress] = useState<string | null>(null);
  const [clonedRepos, setClonedRepos] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);

  const loadRepos = useCallback(async (): Promise<RepoListResponse | null> => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<RepoListResponse>("github_list_repos");
      setSignedIn(result.signed_in);
      setConnected(result.connected);
      setRepos(result.repos ?? []);
      setLoadError(null);
      return result;
    } catch (err) {
      // Do NOT rely on the stale signedIn/connected values to pick a branch —
      // the explicit loadError branch owns rendering on a thrown failure.
      setLoadError(`${err}`);
      return null;
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- one-shot data load on mount
    void loadRepos();
  }, [loadRepos]);

  // --- §4.3 refresh + auto-detect --------------------------------------------
  // Bounded poll after the user clicks "I've connected — refresh": re-check a few
  // times with backoff and stop as soon as the App reports connected. Cancellable.
  const pollTimers = useRef<ReturnType<typeof setTimeout>[]>([]);
  const clearPoll = useCallback(() => {
    for (const t of pollTimers.current) clearTimeout(t);
    pollTimers.current = [];
  }, []);
  const startConnectPoll = useCallback(() => {
    clearPoll();
    // Cumulative wall-clock offsets giving 2s / 4s / 8s gaps between attempts.
    const offsets = [2000, 6000, 14000];
    for (const offset of offsets) {
      const timer = setTimeout(() => {
        void loadRepos().then((result) => {
          if (result?.connected) clearPoll();
        });
      }, offset);
      pollTimers.current.push(timer);
    }
  }, [loadRepos, clearPoll]);

  // Stop polling once connected, and always on unmount.
  useEffect(() => {
    if (connected) clearPoll();
  }, [connected, clearPoll]);
  useEffect(() => clearPoll, [clearPoll]);

  // While the App isn't connected, auto-re-check whenever the window regains
  // focus or the tab becomes visible — so a browser-side connect is detected
  // without the user guessing. Guarded to the not_connected view to avoid
  // spamming the backend from the picker.
  const notConnectedView =
    deriveGithubPickerView({ loading, loadError, signedIn, connected }) === "not_connected";
  useEffect(() => {
    if (!notConnectedView) return;
    const recheck = () => {
      void loadRepos();
    };
    const onVisibility = () => {
      if (document.visibilityState === "visible") recheck();
    };
    window.addEventListener("focus", recheck);
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      window.removeEventListener("focus", recheck);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [notConnectedView, loadRepos]);

  const filteredRepos = useMemo(() => {
    const q = filter.trim().toLowerCase();
    if (!q) return repos;
    return repos.filter(
      (r) =>
        r.nameWithOwner.toLowerCase().includes(q) ||
        (r.description ?? "").toLowerCase().includes(q),
    );
  }, [repos, filter]);

  const pickDestination = useCallback(async () => {
    try {
      const selected = await open({ directory: true, multiple: false });
      if (selected) {
        setDestParent(selected as string);
        setError(null);
      }
    } catch (err) {
      setError(`Failed to open folder picker: ${err}`);
    }
  }, []);

  const toggleRepo = useCallback((nameWithOwner: string) => {
    setSelectedRepos((prev) =>
      prev.includes(nameWithOwner)
        ? prev.filter((r) => r !== nameWithOwner)
        : [...prev, nameWithOwner],
    );
  }, []);

  const cloneSelected = useCallback(async () => {
    if (selectedRepos.length === 0 || !destParent) return;
    setCloning(true);
    setError(null);
    const failures: string[] = [];
    const succeeded: string[] = [];

    for (const repo of selectedRepos) {
      if (clonedRepos.includes(repo)) continue;
      setCloneProgress(`Cloning ${repo}…`);
      try {
        const { path, name } = await invoke<{ path: string; name: string }>("github_clone_repo", {
          repo,
          destParent,
        });

        // Detect the project type from the freshly cloned tree so it flows
        // through the wizard like any locally-scanned project.
        let projects: Project[] = [];
        try {
          projects = await invoke<Project[]>("scan_workspace_for_setup", {
            path,
            maxDepth: 1,
          });
        } catch {
          projects = [];
        }
        if (projects.length === 0) {
          projects = [{ path, name, type: "unknown", manifest: "" }];
        }

        onProjectsCloned(projects);
        succeeded.push(repo);
      } catch (err) {
        failures.push(`${repo}: ${err}`);
      }
    }

    setCloneProgress(null);
    setCloning(false);
    if (succeeded.length > 0) {
      setClonedRepos((prev) => [...prev, ...succeeded]);
      setSelectedRepos((prev) => prev.filter((r) => !succeeded.includes(r)));
    }
    if (failures.length > 0) {
      setError(`Some clones failed:\n${failures.join("\n")}`);
    }
  }, [selectedRepos, destParent, clonedRepos, onProjectsCloned]);

  // --- Render states ---------------------------------------------------------
  // `view` is the single source of truth for which top-level block renders; the
  // early-returns below simply realize each state's markup. See
  // deriveGithubPickerView for the precedence contract.
  const view = deriveGithubPickerView({ loading, loadError, signedIn, connected });

  if (view === "loading") {
    return (
      <div className="flex items-center justify-center gap-2 py-8 text-muted-foreground">
        <Loader2 className="w-4 h-4 animate-spin" />
        Loading your GitHub repositories…
      </div>
    );
  }

  if (view === "error") {
    // Classify the raw backend detail into a specific, actionable headline while
    // always preserving the raw message (muted) for support diagnosis. This is
    // the failure surface — distinct from the genuine "not connected" CTA below.
    // `view === "error"` implies loadError is non-null (see deriveGithubPickerView).
    const headline = classifyLoadError(loadError ?? "");
    return (
      <div className="panel border-red-500/30 bg-red-500/10 p-4 max-w-xl mx-auto w-full flex flex-col gap-3">
        <div className="flex gap-3">
          <AlertCircle className="w-5 h-5 text-red-400 shrink-0 mt-0.5" />
          <div className="text-sm min-w-0">
            <div className="font-medium mb-1 text-red-300">{headline}</div>
            <p className="text-xs text-muted-foreground break-words whitespace-pre-wrap">
              {loadError}
            </p>
          </div>
        </div>
        <div className="flex gap-2">
          <button
            className="btn-secondary flex items-center gap-2"
            onClick={() => void loadRepos()}
          >
            <Loader2 className="w-4 h-4" />
            Retry
          </button>
        </div>
      </div>
    );
  }

  if (view === "signed_out") {
    return (
      <div className="panel border-amber-500/30 bg-amber-500/10 p-4 max-w-xl mx-auto w-full flex gap-3">
        <AlertCircle className="w-5 h-5 text-amber-400 shrink-0 mt-0.5" />
        <div className="text-sm">
          <div className="font-medium mb-1">Sign in required</div>
          <p className="text-muted-foreground">
            Sign in to your Qontinui account (Settings → Account) to browse and clone your GitHub
            repositories.
          </p>
        </div>
      </div>
    );
  }

  if (view === "not_connected") {
    return (
      <div className="panel border-primary/30 bg-primary/5 p-4 max-w-xl mx-auto w-full flex flex-col gap-3">
        <div className="flex gap-3">
          <GitBranch className="w-5 h-5 text-primary shrink-0 mt-0.5" />
          <div className="text-sm">
            <div className="font-medium mb-1">Connect your GitHub</div>
            <p className="text-muted-foreground">
              Install the Qontinui GitHub App on your organization or account to browse and clone
              your repositories here.
            </p>
          </div>
        </div>
        <div className="flex gap-2">
          <button
            className="btn-primary flex items-center gap-2"
            onClick={() => void openUrl(GITHUB_APP_INSTALL_URL)}
          >
            <ExternalLink className="w-4 h-4" />
            Connect GitHub
          </button>
          <button
            className="btn-secondary flex items-center gap-2"
            onClick={() => {
              void loadRepos();
              startConnectPoll();
            }}
          >
            <Loader2 className="w-4 h-4" />
            I've connected — refresh
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-4 max-w-xl mx-auto w-full">
      {/* Filter */}
      <div className="flex-1 flex items-center gap-2 panel px-3 py-2">
        <Search className="w-4 h-4 text-muted-foreground shrink-0" />
        <input
          type="text"
          className="bg-transparent outline-none text-sm flex-1 min-w-0"
          placeholder="Filter repositories"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
      </div>

      {/* Repo list */}
      <div className="flex flex-col gap-1.5 max-h-60 overflow-y-auto pr-1">
        {filteredRepos.length === 0 ? (
          <div className="empty-state py-6 text-center text-muted-foreground text-sm">
            No repositories match
          </div>
        ) : (
          filteredRepos.map((repo) => {
            const isSelected = selectedRepos.includes(repo.nameWithOwner);
            const isCloned = clonedRepos.includes(repo.nameWithOwner);
            return (
              <button
                key={repo.nameWithOwner}
                className={`panel-interactive flex items-center gap-3 p-3 text-left transition-colors ${
                  isSelected ? "border-primary/50" : ""
                } ${isCloned ? "opacity-60" : ""}`}
                onClick={() => !isCloned && toggleRepo(repo.nameWithOwner)}
                disabled={isCloned}
              >
                {isCloned ? (
                  <CheckSquare className="w-4 h-4 text-green-400 shrink-0" />
                ) : isSelected ? (
                  <CheckSquare className="w-4 h-4 text-primary shrink-0" />
                ) : (
                  <Square className="w-4 h-4 text-zinc-500 shrink-0" />
                )}
                <div className="flex-1 min-w-0">
                  <div className="font-medium text-sm flex items-center gap-1.5">
                    {repo.isPrivate ? (
                      <Lock className="w-3 h-3 text-amber-400 shrink-0" />
                    ) : (
                      <Globe className="w-3 h-3 text-muted-foreground shrink-0" />
                    )}
                    <span className="truncate">{repo.nameWithOwner}</span>
                    {isCloned && <span className="text-xs text-green-400">cloned</span>}
                  </div>
                  {repo.description && (
                    <div className="text-xs text-muted-foreground truncate">{repo.description}</div>
                  )}
                </div>
                {repo.language && (
                  <span className="badge text-xs text-zinc-400 shrink-0">{repo.language}</span>
                )}
              </button>
            );
          })
        )}
      </div>

      {/* Destination folder */}
      <div className="flex gap-2 items-center">
        <button
          className="btn-secondary flex items-center gap-2 shrink-0"
          onClick={pickDestination}
        >
          <FolderOpen className="w-4 h-4" />
          Clone to…
        </button>
        <div className="flex-1 panel px-3 py-2 text-sm truncate min-w-0">
          {destParent || <span className="text-muted-foreground">No destination selected</span>}
        </div>
      </div>

      {error && (
        <div className="panel border-red-500/30 bg-red-500/10 p-3 text-sm text-red-300 whitespace-pre-wrap">
          {error}
        </div>
      )}

      {/* Clone action */}
      <button
        className="btn-primary flex items-center justify-center gap-2"
        onClick={cloneSelected}
        disabled={cloning || selectedRepos.length === 0 || !destParent}
      >
        {cloning ? (
          <>
            <Loader2 className="w-4 h-4 animate-spin" />
            {cloneProgress ?? "Cloning…"}
          </>
        ) : (
          <>
            <Download className="w-4 h-4" />
            Clone {selectedRepos.length > 0 ? `${selectedRepos.length} ` : ""}repositor
            {selectedRepos.length === 1 ? "y" : "ies"}
          </>
        )}
      </button>
    </div>
  );
}
