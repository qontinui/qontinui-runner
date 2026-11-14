//! Which AI agent CLI, if any, runs in a terminal pane's process subtree.
//!
//! The "Keep AI" button (`src/components/terminal/pruneTerminals.ts`) closes
//! every pane it cannot prove hosts an AI session. Its tab-level signals —
//! `claudeSessionId`, a Conductor worker id, a remote mirror — are all
//! Claude-shaped, so a pane where the operator started `pi` (DeepSeek), `codex`
//! or `gemini` by hand carried none of them and was closed as a plain shell.
//! The process table does not care which provider is running or how it was
//! launched, so this module answers the question from there.
//!
//! An agent is recognised two ways:
//! - by **image name** — a native or launcher binary (`claude.exe`, `codex`,
//!   `gemini`, `pi`, …), see [`AGENT_IMAGES`];
//! - by **command line** for a process hosted by a script runtime (`node`,
//!   `bun`, `deno`, `python`) — on Windows an npm-installed CLI runs as
//!   `node …\node_modules\@earendil-works\pi-coding-agent\dist\cli.js`, so the
//!   image is `node.exe` and only the argv names the agent. Command lines are
//!   read for exactly those runtime pids, never table-wide.
//!
//! A runtime-hosted match needs the package path, not a bare word: `node
//! server.js --pi` is a dev server, not an agent.
//!
//! Two Linux shapes the Windows ones do not cover, both measured:
//! - Node ≥ 24 renames its main thread, so `/proc/<pid>/comm` — the image name
//!   the `/proc` walk records — reads `MainThread`, not `node`. A runtime is
//!   therefore also recognised by its `/proc/<pid>/exe` basename.
//! - An npm CLI started by its bin name (`pi`, `gemini`) runs through its
//!   `#!/usr/bin/env node` shebang, so the argv is `node <prefix>/bin/pi`: the
//!   bin link, not the package path. A runtime argument that is a `bin/` (or
//!   `.bin/`) entry named exactly like an agent therefore names that agent.

use std::collections::HashMap;

use super::process_tree::{inclusive_subtree_pids, is_countable_claude, ProcessSnapshot};

/// `(image stem, agent kind)` — a process whose image, lowercased with any
/// path and `.exe` stripped, equals the stem IS that agent. `claude` is not
/// here: it goes through [`is_countable_claude`], which excludes qontinui's
/// own identity-shim binaries.
const AGENT_IMAGES: &[(&str, &str)] = &[
    ("codex", "codex"),
    ("gemini", "gemini"),
    ("pi", "pi"),
    ("opencode", "opencode"),
    ("aider", "aider"),
    ("goose", "goose"),
    ("qwen", "qwen"),
    ("crush", "crush"),
    ("cursor-agent", "cursor-agent"),
    ("droid", "droid"),
    ("amp", "amp"),
];

/// Script runtimes that host an npm-installed agent CLI. Python runtimes are
/// matched by shape instead ([`is_runtime_stem`]): `python3.12` is as common
/// as `python3`.
const RUNTIME_IMAGES: &[&str] = &["node", "nodejs", "bun", "deno"];

/// `(package path fragment, agent kind)` matched against a runtime's command
/// line, with `\` normalised to `/` and lowercased. Fragments name the
/// installed package, so an unrelated script mentioning the word does not match.
const AGENT_PACKAGES: &[(&str, &str)] = &[
    ("/pi-coding-agent/", "pi"),
    ("@anthropic-ai/claude-code/", "claude"),
    ("@openai/codex/", "codex"),
    ("@google/gemini-cli/", "gemini"),
    ("/opencode-ai/", "opencode"),
    ("@qwen-code/qwen-code/", "qwen"),
    ("@charmland/crush/", "crush"),
    ("@sourcegraph/amp/", "amp"),
    // The launcher script execs a bundled `…/cursor-agent/versions/<v>/node
    // …/index.js`: neither the image nor a bin entry names it.
    ("/cursor-agent/versions/", "cursor-agent"),
    ("/aider/__main__", "aider"),
    ("/bin/aider", "aider"),
    ("/scripts/aider", "aider"),
];

/// An image name as the checks compare it: last path component, lowercased,
/// `.exe` stripped — and the ` (deleted)` the kernel appends to a
/// `/proc/<pid>/exe` target whose binary was replaced on disk (a package
/// upgrade under a running agent).
fn image_stem(name: &str) -> String {
    let name = name.strip_suffix(" (deleted)").unwrap_or(name);
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase();
    match base.strip_suffix(".exe") {
        Some(stem) => stem.to_string(),
        None => base,
    }
}

/// The agent kind `pid`'s image alone proves, if any.
fn agent_by_image(pid: u32, snapshot: &ProcessSnapshot) -> Option<&'static str> {
    if is_countable_claude(pid, snapshot) {
        return Some("claude");
    }
    let stem = image_stem(snapshot.names.get(&pid)?);
    AGENT_IMAGES
        .iter()
        .find(|(image, _)| *image == stem)
        .map(|(_, kind)| *kind)
}

/// True iff `stem` names a script runtime: one of [`RUNTIME_IMAGES`];
/// `python` / `pypy` followed only by a version and build letters (`python3`,
/// `python3.12`, `python3.13t`, `pythonw`, `pypy3`); or `mainthread`, the
/// `comm` Node ≥ 24 gives itself on Linux. That last one is a thread name, not
/// an image — it only buys an argv read, so when the exe link is unreadable
/// too (another user's process) the pane reads UNKNOWN rather than absent.
fn is_runtime_stem(stem: &str) -> bool {
    RUNTIME_IMAGES.contains(&stem)
        || stem == "mainthread"
        || ["python", "pypy"].iter().any(|p| {
            stem.strip_prefix(p).is_some_and(|v| {
                v.chars()
                    .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'w' | 't'))
            })
        })
}

/// True iff `pid` is a script runtime whose argv must be read — by its image
/// name or by its executable path (whose stem is the real one where the name
/// is a renamed thread).
fn is_runtime(pid: u32, snapshot: &ProcessSnapshot) -> bool {
    [snapshot.names.get(&pid), snapshot.exe_paths.get(&pid)]
        .into_iter()
        .flatten()
        .any(|n| is_runtime_stem(&image_stem(n)))
}

/// The agent kind a bare `bin/` entry names: `<anything>/bin/<stem>` or
/// `<anything>/.bin/<stem>` whose stem is exactly an agent's (`claude`
/// included), so `/usr/bin/pip` is not `pi` and `./pi.js` is no bin entry.
fn agent_by_bin_entry(token: &str) -> Option<&'static str> {
    let path = token.trim_matches(['"', '\'']);
    let (dir, base) = path.rsplit_once('/')?;
    let parent = dir.rsplit('/').next().unwrap_or(dir);
    if parent != "bin" && parent != ".bin" {
        return None;
    }
    if base == "claude" {
        return Some("claude");
    }
    AGENT_IMAGES
        .iter()
        .find(|(image, _)| *image == base)
        .map(|(_, kind)| *kind)
}

/// The agent kind a runtime's command line names, if any: an installed
/// package path ([`AGENT_PACKAGES`]), a `bin/` entry named like an agent
/// ([`agent_by_bin_entry`] — the shebang launch), or `python -m aider`.
pub(crate) fn agent_by_cmdline(cmdline: &str) -> Option<&'static str> {
    let lower = cmdline.to_ascii_lowercase();
    let slashed = lower.replace('\\', "/");
    if let Some(kind) = AGENT_PACKAGES
        .iter()
        .find(|(frag, _)| slashed.contains(frag))
        .map(|(_, kind)| *kind)
    {
        return Some(kind);
    }
    let tokens: Vec<&str> = slashed.split_whitespace().collect();
    if let Some(kind) = tokens.iter().find_map(|t| agent_by_bin_entry(t)) {
        return Some(kind);
    }
    tokens
        .windows(2)
        .any(|w| w[0] == "-m" && (w[1] == "aider" || w[1].starts_with("aider.")))
        .then_some("aider")
}

/// One pane's reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneAgent {
    /// An agent CLI of this kind runs in the subtree.
    Present(&'static str),
    /// The table was read and no agent is in the subtree.
    Absent,
    /// Nothing may be concluded: the table, or a runtime's command line that
    /// could have named an agent (read failed, or access denied for that pid),
    /// was unreadable.
    Unknown,
}

/// Classify each `(pane id, root pid)` from one snapshot, reading command
/// lines only for runtime pids in subtrees no image already settled.
///
/// `read_cmdlines` returns `None` when the read failed (UNKNOWN), mirroring
/// [`super::process_tree::command_lines_for_pids`]; it is a parameter so the
/// decision is unit-testable without a live table.
pub async fn classify_panes<F, Fut>(
    panes: &[(String, u32)],
    snapshot: &ProcessSnapshot,
    read_cmdlines: F,
) -> HashMap<String, PaneAgent>
where
    F: FnOnce(Vec<u32>) -> Fut,
    Fut: std::future::Future<Output = Option<HashMap<u32, String>>>,
{
    let mut out = HashMap::new();
    if snapshot.is_unknown() {
        for (id, _) in panes {
            out.insert(id.clone(), PaneAgent::Unknown);
        }
        return out;
    }

    // Pass 1: images. Collect the runtime pids of every unsettled pane.
    let mut pending: Vec<(String, Vec<u32>)> = Vec::new();
    for (id, root) in panes {
        let subtree = inclusive_subtree_pids(*root, snapshot);
        if let Some(kind) = subtree.iter().find_map(|&p| agent_by_image(p, snapshot)) {
            out.insert(id.clone(), PaneAgent::Present(kind));
            continue;
        }
        let runtimes: Vec<u32> = subtree
            .into_iter()
            .filter(|&p| is_runtime(p, snapshot))
            .collect();
        if runtimes.is_empty() {
            out.insert(id.clone(), PaneAgent::Absent);
        } else {
            pending.push((id.clone(), runtimes));
        }
    }
    if pending.is_empty() {
        return out;
    }

    // Pass 2: one targeted command-line read for every runtime pid.
    let all: Vec<u32> = pending
        .iter()
        .flat_map(|(_, p)| p.iter().copied())
        .collect();
    let cmdlines = read_cmdlines(all).await;
    for (id, runtimes) in pending {
        let verdict = match &cmdlines {
            None => PaneAgent::Unknown,
            Some(map) => {
                if let Some(kind) = runtimes
                    .iter()
                    .filter_map(|p| map.get(p))
                    .find_map(|cl| agent_by_cmdline(cl))
                {
                    PaneAgent::Present(kind)
                } else if runtimes.iter().any(|p| !map.contains_key(p)) {
                    // A runtime whose argv could not be read (access denied —
                    // an elevated pane — or a failed per-pid read) may BE the
                    // agent: that is UNKNOWN, never absent.
                    PaneAgent::Unknown
                } else {
                    PaneAgent::Absent
                }
            }
        };
        out.insert(id, verdict);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(procs: &[(u32, Option<u32>, &str)]) -> ProcessSnapshot {
        let mut s = ProcessSnapshot {
            stale: false,
            ..ProcessSnapshot::default()
        };
        for &(pid, parent, name) in procs {
            s.names.insert(pid, name.to_string());
            s.creation_times.insert(pid, 1);
            s.parent_map.entry(pid).or_default();
            if let Some(pp) = parent {
                s.parent_map.entry(pp).or_default().push(pid);
            }
        }
        s
    }

    fn run(
        panes: &[(&str, u32)],
        s: &ProcessSnapshot,
        cmd: Option<&[(u32, &str)]>,
    ) -> HashMap<String, PaneAgent> {
        let panes: Vec<(String, u32)> = panes.iter().map(|(i, p)| (i.to_string(), *p)).collect();
        let cmd: Option<HashMap<u32, String>> =
            cmd.map(|c| c.iter().map(|(p, l)| (*p, l.to_string())).collect());
        futures::executor::block_on(classify_panes(&panes, s, |_| async move { cmd }))
    }

    /// The reported bug: `pi` launched by hand in a PowerShell pane runs as
    /// `node.exe …\pi-coding-agent\dist\cli.js` and must read as an agent.
    #[test]
    fn pi_hosted_by_node_is_an_agent() {
        let s = snap(&[
            (10, None, "powershell.exe"),
            (11, Some(10), "cmd.exe"),
            (12, Some(11), "node.exe"),
        ]);
        let cl = r#""C:\Program Files\nodejs\node.exe" C:\Users\x\AppData\Roaming\npm\node_modules\@earendil-works\pi-coding-agent\dist\cli.js --provider deepseek"#;
        let out = run(&[("pane", 10)], &s, Some(&[(12, cl)]));
        assert_eq!(out["pane"], PaneAgent::Present("pi"));
    }

    #[test]
    fn native_images_are_agents_and_claude_wins_by_image() {
        let s = snap(&[
            (1, None, "bash"),
            (2, Some(1), "codex"),
            (3, None, "pwsh.exe"),
            (4, Some(3), "claude.exe"),
        ]);
        let out = run(&[("a", 1), ("b", 3)], &s, Some(&[]));
        assert_eq!(out["a"], PaneAgent::Present("codex"));
        assert_eq!(out["b"], PaneAgent::Present("claude"));
    }

    /// A dev server under node is not an agent, and a plain shell is absent.
    #[test]
    fn unrelated_node_and_plain_shell_are_absent() {
        let s = snap(&[(1, None, "bash"), (2, Some(1), "node"), (3, None, "bash")]);
        let out = run(
            &[("dev", 1), ("sh", 3)],
            &s,
            Some(&[(2, "node ./server.js --pi --codex")]),
        );
        assert_eq!(out["dev"], PaneAgent::Absent);
        assert_eq!(out["sh"], PaneAgent::Absent);
    }

    /// An unreadable command line is UNKNOWN for a pane with a runtime, never
    /// absent; a pane with no runtime still reads absent from the table.
    #[test]
    fn unreadable_cmdlines_are_unknown_only_where_they_matter() {
        let s = snap(&[(1, None, "bash"), (2, Some(1), "node"), (3, None, "bash")]);
        let out = run(&[("n", 1), ("sh", 3)], &s, None);
        assert_eq!(out["n"], PaneAgent::Unknown);
        assert_eq!(out["sh"], PaneAgent::Absent);
    }

    /// A runtime the per-pid read could not answer for (elevated `node.exe`)
    /// is UNKNOWN, not absent — it may be the agent.
    #[test]
    fn a_runtime_missing_from_the_cmdline_map_is_unknown() {
        let s = snap(&[(1, None, "pwsh.exe"), (2, Some(1), "node.exe")]);
        let out = run(&[("elevated", 1)], &s, Some(&[]));
        assert_eq!(out["elevated"], PaneAgent::Unknown);
    }

    #[test]
    fn unknown_snapshot_is_unknown_for_every_pane() {
        let out = run(&[("a", 1)], &ProcessSnapshot::default(), Some(&[]));
        assert_eq!(out["a"], PaneAgent::Unknown);
    }

    /// Measured on Linux with Node 24: `comm` is `MainThread`, the exe is
    /// node, and a CLI started by its bin name has argv `node <prefix>/bin/pi`.
    #[test]
    fn linux_node_shebang_launch_is_an_agent() {
        let mut s = snap(&[(1, None, "bash"), (2, Some(1), "MainThread")]);
        s.exe_paths.insert(
            2,
            "/home/u/.nvm/versions/node/v24.19.0/bin/node".to_string(),
        );
        let out = run(
            &[("pane", 1)],
            &s,
            Some(&[(2, "node /home/u/.nvm/versions/node/v24.19.0/bin/pi")]),
        );
        assert_eq!(out["pane"], PaneAgent::Present("pi"));
    }

    /// A renamed-thread runtime whose argv cannot be read is UNKNOWN, not
    /// absent — the exe path is what makes it a runtime at all.
    #[test]
    fn renamed_node_thread_with_unreadable_argv_is_unknown() {
        // A thread name that proves nothing by itself: only the exe path can
        // make this a runtime, so the test pins the exe arm of `is_runtime`.
        let mut s = snap(&[(1, None, "bash"), (2, Some(1), "libuv-worker")]);
        s.exe_paths.insert(2, "/usr/bin/node".to_string());
        let out = run(&[("pane", 1)], &s, Some(&[]));
        assert_eq!(out["pane"], PaneAgent::Unknown);
    }

    /// A Node binary replaced on disk under a running agent: the exe link
    /// reads `… (deleted)`, which must still be node.
    #[test]
    fn replaced_node_binary_is_still_a_runtime() {
        assert_eq!(image_stem("/usr/bin/node (deleted)"), "node");
        // Not `MainThread`, which is a runtime on its own: the exe path must
        // be what decides here.
        let mut s = snap(&[(1, None, "bash"), (2, Some(1), "libuv-worker")]);
        s.exe_paths.insert(2, "/usr/bin/node (deleted)".to_string());
        let out = run(
            &[("pane", 1)],
            &s,
            Some(&[(2, "node /usr/local/bin/gemini")]),
        );
        assert_eq!(out["pane"], PaneAgent::Present("gemini"));
    }

    /// `sudo pi`: the exe link is unreadable (another user's process) and the
    /// name is Node's renamed thread. Still a runtime, so the argv is read.
    #[test]
    fn renamed_thread_with_unreadable_exe_is_still_read() {
        let s = snap(&[(1, None, "bash"), (2, Some(1), "MainThread")]);
        let found = run(&[("pane", 1)], &s, Some(&[(2, "node /usr/local/bin/pi")]));
        assert_eq!(found["pane"], PaneAgent::Present("pi"));
        let unreadable = run(&[("pane", 1)], &s, Some(&[]));
        assert_eq!(unreadable["pane"], PaneAgent::Unknown);
    }

    #[test]
    fn cursor_agent_bundled_node_is_an_agent() {
        assert_eq!(
            agent_by_cmdline("/home/u/.local/share/cursor-agent/versions/2025.11.1/node --use-system-ca /home/u/.local/share/cursor-agent/versions/2025.11.1/index.js"),
            Some("cursor-agent")
        );
    }

    #[test]
    fn python_versions_are_runtimes() {
        for stem in [
            "python",
            "python3",
            "python3.12",
            "python3.13t",
            "pythonw",
            "pypy3",
        ] {
            assert!(is_runtime_stem(stem), "{stem}");
        }
        for stem in ["pythonista", "pi", "nodemon", "pypyx"] {
            assert!(!is_runtime_stem(stem), "{stem}");
        }
    }

    #[test]
    fn bin_entries_and_module_runs_name_agents() {
        assert_eq!(
            agent_by_cmdline("node /usr/local/bin/gemini"),
            Some("gemini")
        );
        assert_eq!(
            agent_by_cmdline("node /home/u/.npm-global/bin/claude --resume"),
            Some("claude")
        );
        assert_eq!(
            agent_by_cmdline("node ./node_modules/.bin/codex"),
            Some("codex")
        );
        assert_eq!(
            agent_by_cmdline("python3.12 -m aider --model x"),
            Some("aider")
        );
        // Exact stems only: `pip` is not `pi`, and a file named like an agent
        // outside a bin dir is a script, not the CLI.
        assert_eq!(agent_by_cmdline("python3 /usr/bin/pip install x"), None);
        assert_eq!(agent_by_cmdline("node ./scripts/pi"), None);
        assert_eq!(agent_by_cmdline("node /srv/bin/pi.js"), None);
    }

    #[test]
    fn cmdline_matching_names_packages_not_words() {
        assert_eq!(
            agent_by_cmdline("node /usr/lib/node_modules/@openai/codex/bin/codex.js"),
            Some("codex")
        );
        assert_eq!(
            agent_by_cmdline(r"node C:\npm\node_modules\@google\gemini-cli\dist\index.js"),
            Some("gemini")
        );
        assert_eq!(agent_by_cmdline("node pi.js"), None);
        assert_eq!(agent_by_cmdline("python -m http.server"), None);
    }
}
