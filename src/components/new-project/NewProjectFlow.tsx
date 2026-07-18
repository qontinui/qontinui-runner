import { useState, useCallback, useEffect, useMemo, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useUIComponent } from "@qontinui/ui-bridge";
import {
  FolderOpen,
  Loader2,
  CheckCircle2,
  AlertCircle,
  ExternalLink,
  Lock,
  Globe,
  HardDrive,
  GitBranch,
  FileCode,
  FilePlus,
  RefreshCw,
  Pencil,
} from "lucide-react";
import { instanceStorage } from "@/lib/instance-storage";
import { useSavedProjects, type SavedProject } from "@/hooks/useSavedProjects";
import {
  NewProjectProgress,
  initialSteps,
  STEP_ORDER,
  type StepName,
  type StepState,
  type StepStatus,
} from "./NewProjectProgress";

// ============================================================================
// Wire types — camelCase, mirror the PR-D Rust contract in
// `src-tauri/src/commands/new_project.rs`.
// ============================================================================

type NameStatus = "available" | "taken" | "invalid" | "owner_unbound";

interface NameCheckResult {
  status: NameStatus;
  reason?: string;
  normalizedName: string;
}

interface GithubOauthStatusResult {
  authorized: boolean;
  authorizeUrl?: string;
}

interface RemoteRepoInfo {
  fullName: string;
  cloneUrl: string;
  htmlUrl: string;
}

interface NewProjectErrorT {
  code: string;
  message: string;
  authorizeUrl?: string;
  field?: string;
}

interface CreateNewProjectResult {
  ok: boolean;
  projectPath: string;
  repo?: RemoteRepoInfo;
  completedSteps: string[];
  failedStep?: string;
  error?: NewProjectErrorT;
  resumeToken?: string;
  warnings: string[];
}

interface ProgressPayload {
  step: string;
  status: StepStatus;
  detail: string;
}

/** Wire shape of `github_list_repos` (reused as the owner-account source). */
interface RepoListResponse {
  signed_in: boolean;
  connected: boolean;
  repos: { nameWithOwner: string }[];
}

type Template = "empty" | "react-ui-bridge";
type Phase = "form" | "creating" | "success" | "error";
type Visibility = "private" | "public";

const PROGRESS_EVENT = "new-project://progress";
const PREFS_KEY = "new-project:prefs";
const OAUTH_POLL_MS = 3000;
const OAUTH_POLL_TIMEOUT_MS = 120_000;

interface Prefs {
  location: string;
  template: Template;
}

function loadPrefs(): Prefs {
  return instanceStorage.getJSON<Prefs>(PREFS_KEY, { location: "", template: "empty" });
}

const TEMPLATES: { id: Template; label: string; description: string; icon: typeof FileCode }[] = [
  {
    id: "empty",
    label: "Empty",
    description: "A minimal repo — README and .gitignore.",
    icon: FilePlus,
  },
  {
    id: "react-ui-bridge",
    label: "React + UI Bridge",
    description: "Vite React + TS with @qontinui/ui-bridge wired and a spec-annotated page.",
    icon: FileCode,
  },
];

const LOCAL_OWNER = "__local__";

export interface NewProjectFlowProps {
  /**
   * Called after a project is successfully created + registered. Receives the
   * persisted `SavedProject` (path / name / projectType / manifest) so the host
   * can flow it into its own selection state (wizard) or logging (modal).
   */
  onCreated?: (project: SavedProject) => void;
  /** Optional close affordance offered by a modal host on the success screen. */
  onClose?: () => void;
}

/**
 * Self-contained "New Project" flow: name (debounced availability check) →
 * location (folder picker) → owner (local-only or a connected GitHub account,
 * with per-owner GitHub-App authorization) → visibility → template → optional
 * coord enrollment → create, driving the eight-step `new-project://progress`
 * checklist. Used both as the wizard's "Create new" mode and inside the
 * Terminal-page `NewProjectModal`.
 */
export function NewProjectFlow({ onCreated, onClose }: NewProjectFlowProps) {
  const { refresh: refreshSavedProjects } = useSavedProjects();

  // --- form state ------------------------------------------------------------
  const [name, setName] = useState("");
  const [nameCheck, setNameCheck] = useState<NameCheckResult | null>(null);
  const [checkingName, setCheckingName] = useState(false);

  const [location, setLocation] = useState<string>(() => loadPrefs().location);
  const [owner, setOwner] = useState<string | null>(null); // null = local only
  const [visibility, setVisibility] = useState<Visibility>("private");
  const [template, setTemplate] = useState<Template>(() => loadPrefs().template);
  const [enroll, setEnroll] = useState(false);

  // --- owner-account source (reused from GithubRepoPicker's `github_list_repos`)
  const [owners, setOwners] = useState<string[]>([]);
  const [accountsSignedIn, setAccountsSignedIn] = useState(true);
  const [accountsConnected, setAccountsConnected] = useState(false);

  // --- GitHub App authorization for the selected owner -----------------------
  const [oauth, setOauth] = useState<GithubOauthStatusResult | null>(null);
  const [oauthChecking, setOauthChecking] = useState(false);
  const [oauthPolling, setOauthPolling] = useState(false);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const pollDeadlineRef = useRef(0);

  // --- creation state --------------------------------------------------------
  const [phase, setPhase] = useState<Phase>("form");
  const [steps, setSteps] = useState<Record<StepName, StepState>>(initialSteps);
  const [result, setResult] = useState<CreateNewProjectResult | null>(null);
  const [fatalError, setFatalError] = useState<string | null>(null);
  const [pickerError, setPickerError] = useState<string | null>(null);

  const nameReqId = useRef(0);
  const oauthReqId = useRef(0);

  // --- effects ---------------------------------------------------------------

  // Load the connected GitHub accounts once. Distinct owners of the tenant's
  // installation repos become the owner-picker options — the same source the
  // setup wizard's GithubRepoPicker uses.
  useEffect(() => {
    let cancelled = false;
    invoke<RepoListResponse>("github_list_repos")
      .then((res) => {
        if (cancelled) return;
        setAccountsSignedIn(res.signed_in);
        setAccountsConnected(res.connected);
        const distinct = Array.from(
          new Set((res.repos ?? []).map((r) => r.nameWithOwner.split("/")[0]).filter(Boolean)),
        ).sort((a, b) => a.localeCompare(b));
        setOwners(distinct);
      })
      .catch(() => {
        // Non-fatal: owner picker degrades to "Local only".
        if (!cancelled) setOwners([]);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Debounced name availability check (~400ms). A monotonic request id drops
  // stale responses so fast typing never flickers an out-of-date badge. All
  // state writes happen inside the timer (never synchronously in the effect
  // body) to keep the render cheap and lint-clean.
  useEffect(() => {
    const raw = name.trim();
    const id = ++nameReqId.current;
    const timer = setTimeout(
      () => {
        if (!raw) {
          setNameCheck(null);
          setCheckingName(false);
          return;
        }
        setCheckingName(true);
        invoke<NameCheckResult>("check_project_name", {
          owner: owner ?? undefined,
          name: raw,
        })
          .then((res) => {
            if (id !== nameReqId.current) return;
            setNameCheck(res);
            setCheckingName(false);
          })
          .catch((err) => {
            if (id !== nameReqId.current) return;
            setNameCheck({ status: "invalid", reason: String(err), normalizedName: "" });
            setCheckingName(false);
          });
      },
      raw ? 400 : 0,
    );
    return () => clearTimeout(timer);
  }, [name, owner]);

  const stopPolling = useCallback(() => {
    if (pollRef.current) {
      clearInterval(pollRef.current);
      pollRef.current = null;
    }
    setOauthPolling(false);
  }, []);

  // Select a GitHub owner (or null = local only) and re-check the App
  // authorization for it. Handled in the change handler rather than an effect
  // (no derived-from-props sync here) — a monotonic request id drops stale
  // status responses if the owner is switched mid-flight.
  const selectOwner = useCallback(
    (next: string | null) => {
      setOwner(next);
      stopPolling();
      setOauth(null);
      if (!next) {
        setOauthChecking(false);
        return;
      }
      const id = ++oauthReqId.current;
      setOauthChecking(true);
      invoke<GithubOauthStatusResult>("github_oauth_status", { owner: next })
        .then((res) => {
          if (id !== oauthReqId.current) return;
          setOauth(res);
          setOauthChecking(false);
        })
        .catch(() => {
          if (id !== oauthReqId.current) return;
          setOauth(null);
          setOauthChecking(false);
        });
    },
    [stopPolling],
  );

  // Persistent progress listener; updates the checklist as the Rust command
  // emits per-step events during a create run.
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    void listen<ProgressPayload>(PROGRESS_EVENT, (event) => {
      const { step, status, detail } = event.payload;
      setSteps((prev) => {
        if (!STEP_ORDER.includes(step as StepName)) return prev;
        return { ...prev, [step as StepName]: { status, detail } };
      });
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  // Stop any OAuth polling on unmount (covers modal close / wizard step change).
  useEffect(() => stopPolling, [stopPolling]);

  // --- derived ---------------------------------------------------------------

  const effectiveName = useMemo(
    () => (nameCheck?.normalizedName || name).trim(),
    [nameCheck, name],
  );

  const normalizedDiffers = useMemo(
    () =>
      !!nameCheck?.normalizedName &&
      nameCheck.normalizedName !== name.trim() &&
      nameCheck.status !== "invalid",
    [nameCheck, name],
  );

  const nameValid = nameCheck?.status === "available";
  const canCreate = nameValid && !checkingName && !!location && phase === "form";

  const resolvedPath = useMemo(() => {
    if (!location) return "";
    const sep = location.includes("\\") ? "\\" : "/";
    const trimmed = location.replace(/[\\/]+$/, "");
    return `${trimmed}${sep}${effectiveName || "…"}`;
  }, [location, effectiveName]);

  // --- actions ---------------------------------------------------------------

  const persistPrefs = useCallback(() => {
    instanceStorage.setJSON(PREFS_KEY, { location, template } satisfies Prefs);
  }, [location, template]);

  const pickLocation = useCallback(async () => {
    try {
      const selected = await open({ directory: true, multiple: false });
      if (selected) {
        setLocation(selected as string);
        setPickerError(null);
      }
    } catch (err) {
      setPickerError(`Failed to open folder picker: ${err}`);
    }
  }, []);

  const startPolling = useCallback(
    (ownerLogin: string) => {
      stopPolling();
      setOauthPolling(true);
      pollDeadlineRef.current = Date.now() + OAUTH_POLL_TIMEOUT_MS;
      pollRef.current = setInterval(() => {
        if (Date.now() > pollDeadlineRef.current) {
          stopPolling();
          return;
        }
        invoke<GithubOauthStatusResult>("github_oauth_status", { owner: ownerLogin })
          .then((res) => {
            setOauth(res);
            if (res.authorized) stopPolling();
          })
          .catch(() => {
            /* keep polling until timeout */
          });
      }, OAUTH_POLL_MS);
    },
    [stopPolling],
  );

  const authorize = useCallback(async () => {
    if (!owner || !oauth?.authorizeUrl) return;
    try {
      await openUrl(oauth.authorizeUrl);
      startPolling(owner);
    } catch (err) {
      setPickerError(`Couldn't open the GitHub authorization page: ${err}`);
    }
  }, [owner, oauth, startPolling]);

  const findCreatedProject = useCallback(
    async (path: string, projectName: string): Promise<SavedProject> => {
      try {
        const all = await invoke<SavedProject[]>("list_saved_projects");
        const match = all.find((p) => p.path === path);
        if (match) return match;
      } catch {
        /* fall through to a reconstructed record */
      }
      return {
        path,
        name: projectName,
        projectType: template === "react-ui-bridge" ? "node" : "unknown",
        manifest: "",
      };
    },
    [template],
  );

  const runCreate = useCallback(
    async (resumeFrom?: string) => {
      const effName = (nameCheck?.normalizedName || name).trim();
      setPhase("creating");
      setFatalError(null);
      setResult(null);
      setSteps(initialSteps());

      const req = {
        name: effName,
        location,
        owner: owner ?? undefined,
        private: visibility === "private",
        template,
        enroll: owner ? enroll : false,
        resumeFrom,
      };

      try {
        const res = await invoke<CreateNewProjectResult>("create_new_project", { req });
        setResult(res);
        if (res.ok) {
          persistPrefs();
          void refreshSavedProjects();
          const created = await findCreatedProject(res.projectPath, effName);
          onCreated?.(created);
          setPhase("success");
        } else {
          setPhase("error");
        }
      } catch (err) {
        // create_new_project only rejects for non-signed-in / bridge errors.
        setFatalError(String(err));
        setPhase("error");
      }
    },
    [
      nameCheck,
      name,
      location,
      owner,
      visibility,
      template,
      enroll,
      persistPrefs,
      refreshSavedProjects,
      findCreatedProject,
      onCreated,
    ],
  );

  // --- UI Bridge state annotation (runner convention: useUIComponent + state)
  useUIComponent({
    id: "new-project-flow",
    name: "New Project Flow",
    description:
      "Create a new project: local folder + optional GitHub repo + coord enrollment. " +
      "State machine phases: form, creating, success, error.",
    actions: [
      {
        id: "back-to-form",
        label: "Back to form",
        description: "Return the flow to the editable form (from an error/success screen).",
        handler: () => {
          setPhase("form");
        },
      },
    ],
    state: () => ({
      phase,
      name,
      normalizedName: nameCheck?.normalizedName ?? "",
      nameStatus: nameCheck?.status ?? null,
      checkingName,
      location,
      owner,
      visibility,
      template,
      enroll,
      oauthAuthorized: oauth?.authorized ?? null,
      failedStep: result?.failedStep ?? null,
      errorCode: result?.error?.code ?? null,
    }),
  });

  // ==========================================================================
  // Render
  // ==========================================================================

  if (phase === "success") {
    return (
      <div className="flex flex-col gap-5">
        <div className="flex flex-col items-center text-center gap-2 py-2">
          <CheckCircle2 className="w-10 h-10 text-green-400" />
          <h3 className="text-lg font-semibold">Project created</h3>
          <p className="text-sm text-muted-foreground break-all">{result?.projectPath}</p>
        </div>

        <NewProjectProgress steps={steps} />

        {result?.warnings && result.warnings.length > 0 && (
          <div className="panel border-amber-500/30 bg-amber-500/10 p-3 text-xs text-amber-200 whitespace-pre-wrap">
            {result.warnings.join("\n")}
          </div>
        )}

        <div className="flex items-center justify-between gap-2">
          {result?.repo?.htmlUrl ? (
            <button
              className="btn-secondary flex items-center gap-2"
              onClick={() => void openUrl(result.repo!.htmlUrl)}
            >
              <GitBranch className="w-4 h-4" />
              Open on GitHub
              <ExternalLink className="w-3.5 h-3.5" />
            </button>
          ) : (
            <span />
          )}
          <button className="btn-primary" onClick={() => (onClose ? onClose() : setPhase("form"))}>
            Done
          </button>
        </div>
      </div>
    );
  }

  if (phase === "creating" || phase === "error") {
    const err = result?.error;
    return (
      <div className="flex flex-col gap-5">
        <div className="text-center">
          <h3 className="text-lg font-semibold">
            {phase === "creating" ? "Creating project…" : "Project creation stopped"}
          </h3>
          <p className="text-sm text-muted-foreground break-all">
            {result?.projectPath || resolvedPath}
          </p>
        </div>

        <NewProjectProgress steps={steps} />

        {(err || fatalError) && (
          <div className="panel border-red-500/30 bg-red-500/10 p-3 flex flex-col gap-2">
            <div className="flex gap-2">
              <AlertCircle className="w-4 h-4 text-red-400 shrink-0 mt-0.5" />
              <div className="text-sm text-red-200 min-w-0">
                {result?.failedStep && (
                  <div className="font-medium">Failed at: {result.failedStep}</div>
                )}
                <p className="break-words whitespace-pre-wrap">{err?.message || fatalError}</p>
              </div>
            </div>
            {err?.code === "user_authorization_required" && err.authorizeUrl && (
              <button
                className="btn-secondary flex items-center gap-2 self-start"
                onClick={() => void openUrl(err.authorizeUrl!)}
              >
                <ExternalLink className="w-4 h-4" />
                Authorize GitHub…
              </button>
            )}
          </div>
        )}

        {phase === "error" && (
          <div className="flex items-center justify-between gap-2">
            <button
              className="btn-secondary flex items-center gap-2"
              onClick={() => setPhase("form")}
            >
              <Pencil className="w-4 h-4" />
              Edit details
            </button>
            {/* A name conflict can't be resumed — the name must change first, so
                steer to "Edit details" rather than a doomed retry. All other
                failures (auth, network, push) are resumable from the token. */}
            {result?.resumeToken && result.error?.field !== "name" && (
              <button
                className="btn-primary flex items-center gap-2"
                onClick={() => void runCreate(result.resumeToken)}
              >
                <RefreshCw className="w-4 h-4" />
                Retry remaining steps
              </button>
            )}
          </div>
        )}
      </div>
    );
  }

  // phase === "form"
  const ownerValue = owner ?? LOCAL_OWNER;

  return (
    <div className="flex flex-col gap-5">
      {/* Name */}
      <div className="flex flex-col gap-1.5">
        <label className="text-label text-muted-foreground">Project name</label>
        <div className="flex items-center gap-2 panel px-3 py-2">
          <input
            type="text"
            autoFocus
            className="bg-transparent outline-none text-sm flex-1 min-w-0"
            placeholder="my-new-project"
            value={name}
            onChange={(e) => setName(e.target.value)}
          />
          {checkingName && <Loader2 className="w-4 h-4 animate-spin text-muted-foreground" />}
          {!checkingName && nameCheck && <NameBadge status={nameCheck.status} />}
        </div>
        {!checkingName && nameCheck?.status === "invalid" && nameCheck.reason && (
          <p className="text-xs text-red-300">{nameCheck.reason}</p>
        )}
        {!checkingName && nameCheck?.status === "taken" && (
          <p className="text-xs text-red-300">
            {nameCheck.reason || "A repository with this name already exists."}
          </p>
        )}
        {!checkingName && nameCheck?.status === "owner_unbound" && (
          <p className="text-xs text-amber-300">
            {nameCheck.reason ||
              "This GitHub account isn't connected to Qontinui. Connect it in the setup wizard."}
          </p>
        )}
        {!checkingName && normalizedDiffers && (
          <p className="text-xs text-amber-300">
            Will be created as <span className="font-mono">{nameCheck?.normalizedName}</span>{" "}
            (GitHub-safe name).
          </p>
        )}
      </div>

      {/* Location */}
      <div className="flex flex-col gap-1.5">
        <label className="text-label text-muted-foreground">Location</label>
        <div className="flex gap-2 items-center">
          <button className="btn-secondary flex items-center gap-2 shrink-0" onClick={pickLocation}>
            <FolderOpen className="w-4 h-4" />
            Browse
          </button>
          <div className="flex-1 panel px-3 py-2 text-sm truncate min-w-0">
            {location || <span className="text-muted-foreground">No folder selected</span>}
          </div>
        </div>
        {resolvedPath && (
          <p className="text-xs text-muted-foreground">
            Creates <span className="font-mono break-all">{resolvedPath}</span>
          </p>
        )}
      </div>

      {/* Owner */}
      <div className="flex flex-col gap-1.5">
        <label className="text-label text-muted-foreground">GitHub owner</label>
        <select
          className="input"
          value={ownerValue}
          onChange={(e) => selectOwner(e.target.value === LOCAL_OWNER ? null : e.target.value)}
        >
          <option value={LOCAL_OWNER}>Local only (no GitHub)</option>
          {owners.map((o) => (
            <option key={o} value={o}>
              {o}
            </option>
          ))}
        </select>
        {owner === null && owners.length === 0 && accountsSignedIn && !accountsConnected && (
          <p className="text-xs text-muted-foreground flex items-center gap-1">
            <HardDrive className="w-3 h-3" />
            No GitHub account connected — connect one in the setup wizard to create remote repos.
          </p>
        )}
        {owner !== null && oauthChecking && (
          <p className="text-xs text-muted-foreground flex items-center gap-1">
            <Loader2 className="w-3 h-3 animate-spin" />
            Checking GitHub authorization…
          </p>
        )}
        {owner !== null && !oauthChecking && oauth && !oauth.authorized && (
          <div className="panel border-amber-500/30 bg-amber-500/10 p-3 flex flex-col gap-2">
            <p className="text-xs text-amber-200">
              Qontinui needs your authorization to create repositories on{" "}
              <span className="font-medium">{owner}</span>.
            </p>
            <div className="flex items-center gap-2">
              <button
                className="btn-secondary flex items-center gap-2"
                onClick={() => void authorize()}
                disabled={!oauth.authorizeUrl}
              >
                <ExternalLink className="w-4 h-4" />
                Authorize GitHub…
              </button>
              {oauthPolling && (
                <span className="text-xs text-muted-foreground flex items-center gap-1">
                  <Loader2 className="w-3 h-3 animate-spin" />
                  Waiting for authorization…
                </span>
              )}
            </div>
          </div>
        )}
      </div>

      {/* Visibility (GitHub only) */}
      {owner !== null && (
        <div className="flex flex-col gap-1.5">
          <label className="text-label text-muted-foreground">Visibility</label>
          <div className="flex gap-2">
            <button
              className={`btn-secondary flex items-center gap-2 ${
                visibility === "private" ? "border-primary/50 text-primary" : ""
              }`}
              onClick={() => setVisibility("private")}
            >
              <Lock className="w-4 h-4" />
              Private
            </button>
            <button
              className={`btn-secondary flex items-center gap-2 ${
                visibility === "public" ? "border-primary/50 text-primary" : ""
              }`}
              onClick={() => setVisibility("public")}
            >
              <Globe className="w-4 h-4" />
              Public
            </button>
          </div>
        </div>
      )}

      {/* Template */}
      <div className="flex flex-col gap-1.5">
        <label className="text-label text-muted-foreground">Template</label>
        <div className="grid grid-cols-2 gap-2">
          {TEMPLATES.map((tpl) => {
            const Icon = tpl.icon;
            const selected = template === tpl.id;
            return (
              <button
                key={tpl.id}
                className={`panel-interactive flex flex-col items-start gap-1 p-3 text-left ${
                  selected ? "border-primary/50" : ""
                }`}
                onClick={() => setTemplate(tpl.id)}
              >
                <div className="flex items-center gap-2 font-medium text-sm">
                  <Icon
                    className={`w-4 h-4 ${selected ? "text-primary" : "text-muted-foreground"}`}
                  />
                  {tpl.label}
                </div>
                <p className="text-xs text-muted-foreground">{tpl.description}</p>
              </button>
            );
          })}
        </div>
      </div>

      {/* Enroll (GitHub only) */}
      {owner !== null && (
        <label className="flex items-start gap-2.5 cursor-pointer panel p-3">
          <input
            type="checkbox"
            className="mt-0.5"
            checked={enroll}
            onChange={(e) => setEnroll(e.target.checked)}
          />
          <span className="text-sm">
            <span className="font-medium">Enable Qontinui management</span>
            <span className="block text-xs text-muted-foreground">
              Enroll the new repo with the Qontinui GitHub Apps after the first push (coord-managed
              from day one).
            </span>
          </span>
        </label>
      )}

      {pickerError && (
        <div className="panel border-red-500/30 bg-red-500/10 p-3 text-sm text-red-300 whitespace-pre-wrap">
          {pickerError}
        </div>
      )}

      {/* Create */}
      <button
        className="btn-primary flex items-center justify-center gap-2"
        onClick={() => void runCreate()}
        disabled={!canCreate}
      >
        <FilePlus className="w-4 h-4" />
        Create project
      </button>
    </div>
  );
}

function NameBadge({ status }: { status: NameStatus }) {
  switch (status) {
    case "available":
      return (
        <span className="badge text-xs text-green-400 flex items-center gap-1">
          <CheckCircle2 className="w-3.5 h-3.5" />
          Available
        </span>
      );
    case "taken":
      return (
        <span className="badge text-xs text-red-400 flex items-center gap-1">
          <AlertCircle className="w-3.5 h-3.5" />
          Taken
        </span>
      );
    case "owner_unbound":
      return (
        <span className="badge text-xs text-amber-400 flex items-center gap-1">
          <AlertCircle className="w-3.5 h-3.5" />
          Not connected
        </span>
      );
    default:
      return (
        <span className="badge text-xs text-red-400 flex items-center gap-1">
          <AlertCircle className="w-3.5 h-3.5" />
          Invalid
        </span>
      );
  }
}
