/**
 * File Conflict Detection Tutorial
 *
 * Informational tutorial explaining the advisory file registry that tracks
 * files under active development across concurrent sessions. Covers what
 * the UI shows, how auto-registration works, and how the system prevents
 * merge conflicts during multi-session development.
 */

import type { Tutorial } from "../../../types/tutorial";

export const fileConflictDetectionTutorial: Tutorial = {
  id: "file-conflict-detection",
  title: "File Conflict Detection for Concurrent Sessions",
  description:
    "Understand how the runner tracks files under active development across multiple concurrent sessions and warns you about potential merge conflicts.",
  duration: "8 minutes",
  difficulty: "intermediate",
  mode: "contextual",
  focusPage: "help",
  category: "Architecture",
  tags: ["file-registry", "concurrent-sessions", "conflict-detection", "terminal", "featured"],
  prerequisites: ["getting-started"],
  learningObjectives: [
    "Understand why file conflict detection exists and the problem it solves",
    "Know what the FileConflictBanner shows and when it appears",
    "Understand how files are auto-registered when Claude Code edits them",
    "Know how conflict warnings are injected into session prompts",
    "See how per-session conflict indicators work on SessionCards",
    "Understand the full lifecycle: registration, detection, warning, cleanup",
  ],
  steps: [
    {
      id: "problem",
      title: "The Problem: Concurrent Sessions, Same Files",
      content: `When you run **multiple AI sessions simultaneously** (workflows and terminal sessions), they often work on the same branch for efficiency. But if two sessions edit the same file at the same time, you get **merge conflicts** that are tedious to resolve.

**The file registry solves this** by tracking which files each session is actively editing. When overlap is detected:

- New sessions are **warned in their prompt** to avoid those files
- The Terminal page shows a **conflict banner** listing shared files
- Each session card shows a **warning icon** with the conflict count

The system is **advisory, not blocking** — sessions can still edit conflicting files if needed, but they're informed about the risk.`,
      estimatedDuration: 1,
      tips: [
        "This is especially valuable when running 5+ sessions concurrently on the same branch",
        "The registry is in-memory only — it resets when the runner restarts",
      ],
    },
    {
      id: "auto-registration",
      title: "How Files Get Registered",
      content: `Files are **automatically registered** — you don't need to do anything manually.

When Claude Code uses the **Edit** or **Write** tool during a session, the runner's dispatcher intercepts the tool_use block from the stream-json output and registers the file path in the registry.

**The flow:**
1. Claude Code calls \`Edit { file_path: "src/main.rs" }\`
2. The dispatcher parses the tool_use from stdout
3. \`FileRegistryManager.register()\` is called with the file path and session ID
4. If another session already holds that file, a **conflict event** is emitted

This happens in real-time as the session works — there's no batch or delay.`,
      estimatedDuration: 1,
      details: `**Technical detail:** The dispatcher in \`claude_session/dispatcher.rs\` hooks into the existing tool activity parsing. When it sees an Edit or Write tool_use, it calls \`auto_register_file()\` which:

1. Gets the \`file_registry_manager\` from AppState via \`app_handle.try_state()\`
2. Spawns an async task (to avoid blocking the synchronous dispatcher)
3. Calls \`registry.register()\` which returns any conflicts
4. If conflicts exist, emits both a Tauri event and a WebSocket broadcast

The file path is normalized: backslashes become forward slashes, and on Windows paths are lowercased for case-insensitive matching.`,
    },
    {
      id: "conflict-banner",
      title: "The Conflict Banner",
      content: `The **FileConflictBanner** appears at the top of the Terminal page, just below the notification bar. It has two modes:

**1. Real-time toast alert** — When a conflict is first detected (session B edits a file session A already holds), an amber alert appears:
> *"Conflict: src/main.rs is also being edited by Session Alpha"*

This auto-dismisses after 10 seconds, or you can click the X to dismiss it immediately.

**2. Persistent summary** — A collapsible line shows the total count:
> *"3 files under concurrent development"*

Click it to expand and see each file with the sessions holding it:
> - \`src/main.rs\` (Session Alpha, Session Beta)
> - \`src/lib.rs\` (Session Alpha)`,
      estimatedDuration: 1,
      tips: [
        "The banner uses amber (#e0af68) styling, matching the 'needs-input' status color",
        "If there are no conflicts, the banner renders nothing — it's completely invisible",
        "The registry is polled every 30 seconds, plus instantly on real-time conflict events",
      ],
    },
    {
      id: "session-card-indicator",
      title: "Per-Session Conflict Icons",
      content: `Each **SessionCard** in the sidebar shows a small warning icon when that session has files overlapping with other sessions.

The indicator appears as a **FileWarning icon** in amber with a numeric count badge — for example, **"2"** means that session shares 2 files with other active sessions.

Hovering shows a tooltip: *"2 files shared with other sessions"*

This lets you quickly scan the session list and see which sessions are at risk of conflicts without expanding the banner.`,
      estimatedDuration: 1,
      details: `**How the count is computed:**

The \`useFileConflicts\` hook derives a \`sessionConflictCounts\` Map from the registry data:
1. Groups all registry entries by file path
2. For files held by 2+ distinct sessions, each holder gets +1 to their conflict count
3. The Map is keyed by \`task_run_id\` (which equals \`session.sessionId\`)

This is passed through \`SessionManagerPanel\` to each \`SessionCard\` as the \`fileConflictCount\` prop.`,
    },
    {
      id: "prompt-injection",
      title: "Prompt Warnings for AI Sessions",
      content: `When a **new session starts**, the runner checks the file registry and **injects a warning section** into the prompt if conflicts exist:

\`\`\`
## Active File Conflicts Warning

The following files are currently being worked on
by other active sessions. Avoid modifying these
files to prevent merge conflicts:

- **src/main.rs** (active in: 'Session Alpha')
- **src/lib.rs** (active in: 'Session Alpha', 'Session Beta')

If you must edit these files, coordinate with the
other session(s) first.
\`\`\`

This is prepended to the prompt before other context sections. The AI sees the warning and can make informed decisions about which files to touch.`,
      estimatedDuration: 1,
      tips: [
        "Workflow agentic phases also get this injection — not just terminal sessions",
        "The warning uses the session's task_run_id to exclude itself from the conflict list",
      ],
    },
    {
      id: "lifecycle",
      title: "Registration Lifecycle: Create to Cleanup",
      content: `File registrations follow the session lifecycle automatically:

**Registration** — Files are added when Claude Code edits them (auto-registration via dispatcher)

**Active period** — Registry entries persist while the session is running. The banner and session card icons update in real-time.

**Cleanup** — Entries are released when the session ends, through ANY of these paths:
- Task stopped (Stop button or Stop All)
- Task completed successfully
- Task failed
- WorkflowDropGuard fires (safety net for aborted tasks)
- Periodic 60-second stale sweep (catches any leaks)

You never need to manually clean up — the system handles it.`,
      estimatedDuration: 1,
      details: `**All cleanup integration points:**

| Path | Location |
|------|----------|
| Stop endpoint | \`mcp/task_runs.rs\` |
| GraphQL stop | \`graphql/mutation.rs\` |
| Stop All | \`mcp/ai_session.rs\` (stop_ai_analysis) |
| Completed | \`task_lifecycle.rs\` (mark_task_completed) |
| Failed | \`task_lifecycle.rs\` (mark_task_failed) |
| Aborted | \`WorkflowDropGuard\` Drop impl |
| Stale sweep | \`mcp_api.rs\` (60s background task) |

The stale sweep queries PG for task status — if a task is no longer "running", its registry entries are removed.`,
    },
    {
      id: "api-endpoints",
      title: "MCP API Endpoints",
      content: `The file registry exposes **5 HTTP endpoints** for programmatic access:

| Endpoint | Method | Purpose |
|----------|--------|---------|
| \`/file-registry/register\` | POST | Register files, get conflicts back |
| \`/file-registry/unregister\` | POST | Release specific files |
| \`/file-registry/release-all\` | POST | Release all files for a session |
| \`/file-registry/check-conflicts\` | POST | Query conflicts (all or specific files) |
| \`/file-registry/info\` | GET | Full registry snapshot |

The \`/register\` endpoint is the most interesting — it's idempotent (same session re-registering the same file is a no-op) and returns any conflicts in the response, so the caller knows immediately if overlap exists.`,
      estimatedDuration: 1,
      tips: [
        "The frontend polls /file-registry/info every 30 seconds for the full snapshot",
        "The /check-conflicts endpoint takes an optional file_paths array to check specific files only",
      ],
    },
    {
      id: "architecture-summary",
      title: "Architecture Summary",
      content: `The file registry follows the **UrlLockManager pattern** but is advisory instead of exclusive:

**Core:** \`FileRegistryManager\` in \`executor/file_registry.rs\`
- \`Arc<RwLock<HashMap<path, Vec<entry>>>>\` — multiple sessions per file
- 19 unit tests covering registration, conflicts, cleanup, serialization

**MCP API:** \`mcp/file_registry.rs\` — 5 thin HTTP handlers over the core

**Auto-registration:** \`claude_session/dispatcher.rs\` — intercepts Edit/Write tool_use

**Prompt injection:** \`mcp/ai_session.rs\` + \`phases/agentic.rs\`

**Frontend:** \`useFileConflicts.ts\` hook + \`FileConflictBanner.tsx\` + \`SessionCard\` integration

**Events:** Tauri \`file-conflict-detected\` event + WebSocket broadcast

The entire system is **in-memory only** — no database tables, no persistence. It resets cleanly on runner restart, which is appropriate since file locks are inherently ephemeral.`,
      estimatedDuration: 1,
      resources: [
        { title: "FileRegistryManager source", url: "executor/file_registry.rs" },
        { title: "MCP API endpoints", url: "mcp/file_registry.rs" },
        { title: "Auto-registration dispatcher", url: "claude_session/dispatcher.rs" },
        { title: "Frontend hook", url: "components/terminal/useFileConflicts.ts" },
      ],
    },
  ],
};
