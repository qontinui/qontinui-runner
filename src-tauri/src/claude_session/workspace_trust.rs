//! Pre-accept the Claude **workspace trust** dialog for a directory we are
//! about to spawn a session in, so an autonomous session never stalls on it.
//!
//! ## Why this exists
//!
//! Workspace trust is a SEPARATE gate from permission prompts. Neither
//! `--permission-mode bypassPermissions` nor `--dangerously-skip-permissions`
//! (nor the `skipDangerousModePermissionPrompt` setting) suppresses it, so an
//! autonomous spawn into a not-yet-trusted directory hangs forever on a TUI
//! prompt no one is there to answer. In non-interactive (`--print`) mode it does
//! not hang, but the CLI silently *ignores* the workspace's hooks and MCP
//! servers — "this workspace has not been trusted" — which is the quieter and
//! nastier half of the same failure.
//!
//! ## The contract we are writing against
//!
//! Trust is persisted per ACCOUNT, in `$CLAUDE_CONFIG_DIR/.claude.json`, as
//! `projects[<key>].hasTrustDialogAccepted = true`. Two properties of `<key>`
//! are load-bearing:
//!
//! 1. **The key is the enclosing git root, not the cwd.** A session started in
//!    `<repo>/src-tauri/src` keys to `<repo>`. Only when the directory is in no
//!    git repository at all is the key the directory itself. See
//!    [`project_key`].
//! 2. **Trust inheritance stops at that git root.** The CLI walks parent
//!    directories looking for an accepted entry, but refuses to ascend past the
//!    git root, so trusting a workspace root does NOT cover the repos checked
//!    out beneath it — each repo, and each linked worktree (whose `.git` is a
//!    *file*, so it is its own root), needs its own entry.
//!
//! **Evidence for (2)**, since it is the claim that justifies writing per spawn
//! rather than backfilling once. Two independent sources, recorded here because
//! a design note is not evidence:
//!
//! - *Observed*: on the operator box, `D:/qontinui-root` was present and
//!   trusted in the `.claude-hotmail` account while a session spawned into
//!   `D:/qontinui-root/ui-bridge` — a git repo beneath it — still raised the
//!   trust dialog. That entry existed with the flag explicitly `false`.
//! - *Mechanism*: in the 2.1.251 CLI the ancestor walk is bounded by the git
//!   root. The lookup resolves the git root first and only searches at or below
//!   it; a negative git-root probe is a distinct sentinel from "no entry", and
//!   the walk terminates when the candidate is no longer under that root.
//!
//! Property (1) is corroborated by the same corpus: across the machine's
//! account configs, every existing project key was either a git-repo root or a
//! directory in no repo — never a subdirectory of a repo.
//!
//! ## Safety posture
//!
//! `.claude.json` is live, mutable, ~70-120KB of real account state (OAuth
//! account, MCP servers, per-project history) that every running CLI session
//! rewrites. The CLI itself takes no lock on it. This module therefore:
//!
//! - **never writes when the flag is already set** — the common case after the
//!   first spawn into a repo, which keeps writes rare rather than per-spawn;
//! - **never writes a file it could not parse** — an unreadable or non-object
//!   config is left strictly alone rather than replaced with a "repaired" one;
//! - **writes to a UNIQUE temp file** in the same directory and renames it over
//!   the target, so a concurrent reader sees either the old file or the new one
//!   and two concurrent writers cannot interleave through one path;
//! - **fsyncs before the rename**, so a crash cannot leave a short file
//!   installed over live account state;
//! - **skips accounts with no `.claude.json`** — absence means the account has
//!   never been set up, and inventing one would only half-create it.
//!
//! ### What this does NOT guarantee
//!
//! Two honest limits, both consequences of the CLI exposing no lock:
//!
//! - **The write is a whole-document replace, not a leaf update.** We edit one
//!   leaf of a parsed document, but we then write the whole file. A CLI write
//!   landing inside our read→rename window loses everything that write
//!   contained. `stat`-based change detection immediately before the rename
//!   narrows the window but cannot close it.
//! - **Key ORDER is not preserved.** `serde_json::Value` is backed by a
//!   `BTreeMap` here, so the rewritten document is alphabetized. Values survive
//!   verbatim; layout and ordering do not.
//!
//! The exposure is bounded by writing only the account a spawn actually
//! resolved wherever that is known ([`TrustTargets::Account`]), and by the
//! skip-when-already-set rule. [`TrustTargets::EveryKnownAccount`] is for the
//! one surface that cannot know the account yet, and multiplies the write count
//! by the roster size — use it deliberately.
//!
//! ### On the shape of a newly-created entry
//!
//! A key we add carries exactly `{"hasTrustDialogAccepted": true}` rather than
//! the ~10-key object the CLI writes for a project it has opened. That minimal
//! shape is producer-sanctioned, not invented: the CLI's own cloud provisioning
//! path seeds pre-trusted repos with exactly this one-key object, and the CLI
//! fills the remaining keys from its defaults on first use.
//!
//! Every failure here is best-effort and non-fatal: a spawn that cannot be
//! pre-trusted is still a spawn, it just may face the dialog.

use std::io::Write;
use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

/// The config filename holding per-project trust, inside a `CLAUDE_CONFIG_DIR`.
pub(crate) const CONFIG_FILE: &str = ".claude.json";

/// The `projects` map key, and the trust flag within one project's entry.
pub(crate) const PROJECTS_KEY: &str = "projects";
pub(crate) const TRUST_FLAG: &str = "hasTrustDialogAccepted";

/// Which account configs a pre-trust should reach.
#[derive(Debug, Clone, Copy)]
pub enum TrustTargets<'a> {
    /// Exactly the account this spawn resolved — `None` meaning the child will
    /// inherit the ambient default (`~/.claude.json`). Preferred: it writes one
    /// file, and it writes the file the child will actually read.
    Account(Option<&'a str>),
    /// Every account this machine knows about, plus the ambient default. For
    /// the one surface where the account is chosen *after* us: the frontend
    /// composes its own `CLAUDE_CONFIG_DIR=… claude …` line and types it into a
    /// PTY, so at terminal-creation time the directory is known and the account
    /// is not.
    EveryKnownAccount,
}

/// What happened for one (config file, project key) pair. Returned for logging
/// and for the unit tests; no caller branches on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustOutcome {
    /// The flag was already `true` — no write was attempted.
    AlreadyTrusted,
    /// The flag was absent or false and has been set to `true`.
    Trusted,
    /// Deliberately left alone; the reason is a fixed, greppable label.
    Skipped(&'static str),
    /// An IO or serialization failure. Non-fatal; the spawn proceeds.
    Failed(String),
}

/// Render a resolved path the way the CLI keys it: forward slashes, no Windows
/// verbatim prefix.
///
/// Both verbatim forms must be handled. `canonicalize` returns `\\?\D:\x` for a
/// drive path and `\\?\UNC\server\share` for a network path; the latter must
/// become `//server/share`, NOT `UNC/server/share`. Drive-letter CASE is
/// preserved deliberately — the CLI does not fold it, so neither may we.
fn to_key_string(path: &Path) -> String {
    let s = path.to_string_lossy().into_owned();
    let s = if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
    };
    s.replace('\\', "/")
}

/// Walk `start` and its ancestors for a `.git` entry, returning the directory
/// that holds it.
///
/// A `.git` **file** counts: that is how a linked worktree marks its root, and
/// treating it as a root is exactly what makes each agent worktree its own
/// trust key. Returns `None` when no ancestor has one — the "not in a git repo"
/// case, where the key is the directory itself.
fn git_root(start: &Path) -> Option<PathBuf> {
    let mut cur = Some(start);
    while let Some(dir) = cur {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    None
}

/// The `projects` key the CLI will look up for a session whose cwd is `dir`:
/// the canonicalized enclosing git root, else the canonicalized directory.
///
/// `None` when `dir` is relative or does not resolve. Both cases are refusals
/// rather than guesses: a relative path would silently resolve against the
/// *runner's* cwd, and either way we would write a permanent key into an
/// account file that the CLI is never going to look up.
pub fn project_key(dir: &Path) -> Option<String> {
    if !dir.is_absolute() {
        return None;
    }
    let resolved = std::fs::canonicalize(dir).ok()?;
    let root = git_root(&resolved).unwrap_or(resolved);
    Some(to_key_string(&root))
}

/// Set `projects[key].hasTrustDialogAccepted = true` in one account's config
/// file, atomically and idempotently. See the module docs for the rules this
/// enforces; each is a distinct [`TrustOutcome`] variant.
pub fn ensure_trusted_in(config_file: &Path, key: &str) -> TrustOutcome {
    let raw = match std::fs::read_to_string(config_file) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // The account has never run; the CLI creates this file itself at
            // first launch. Half-creating it here would be worse than the
            // dialog we are trying to avoid.
            return TrustOutcome::Skipped("no config file");
        }
        Err(e) => return TrustOutcome::Failed(format!("read: {e}")),
    };

    // Never overwrite a config we could not parse — that is someone else's
    // half-written state, or a format we do not understand.
    let mut doc: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => return TrustOutcome::Skipped(leak_parse_reason(e)),
    };
    let Some(root) = doc.as_object_mut() else {
        return TrustOutcome::Skipped("config is not a JSON object");
    };

    let projects = root
        .entry(PROJECTS_KEY)
        .or_insert_with(|| serde_json::Value::Object(Default::default()));
    let Some(projects) = projects.as_object_mut() else {
        return TrustOutcome::Skipped("projects is not a JSON object");
    };

    let entry = projects
        .entry(key)
        .or_insert_with(|| serde_json::Value::Object(Default::default()));
    let Some(entry) = entry.as_object_mut() else {
        return TrustOutcome::Skipped("project entry is not a JSON object");
    };

    if entry.get(TRUST_FLAG).and_then(|v| v.as_bool()) == Some(true) {
        return TrustOutcome::AlreadyTrusted;
    }
    entry.insert(TRUST_FLAG.to_string(), serde_json::Value::Bool(true));

    match write_atomically(config_file, &doc, raw.len()) {
        Ok(bytes) => {
            info!(
                config = %config_file.display(),
                project = %key,
                bytes,
                "workspace trust: pre-accepted (rewrote account config)"
            );
            TrustOutcome::Trusted
        }
        Err(e) => TrustOutcome::Failed(e),
    }
}

/// A parse failure is a `Skipped`, not a `Failed`: the file is left strictly
/// alone, which is the documented "never write what we could not parse" rule.
/// The label is fixed so it stays greppable.
fn leak_parse_reason(_e: serde_json::Error) -> &'static str {
    "config did not parse"
}

/// Serialize `doc` to a UNIQUE sibling temp file, fsync it, then rename it over
/// `target`. Returns the byte count written.
///
/// The temp file must be unique per *call*, not per process: several threads in
/// one runner (the terminal pool, the axum handlers, each spawn path) can reach
/// the same account directory at once, and a shared temp path lets one thread's
/// rename publish another thread's half-written document.
///
/// `expected_len` is the length the file had when we read it. If it differs
/// immediately before the rename, another writer got there first and our
/// document is stale — abort rather than clobber it. This narrows the
/// lost-update window; it does not close it.
fn write_atomically(
    target: &Path,
    doc: &serde_json::Value,
    expected_len: usize,
) -> Result<usize, String> {
    let dir = target
        .parent()
        .ok_or_else(|| "config path has no parent".to_string())?;
    let body = serde_json::to_vec_pretty(doc).map_err(|e| format!("serialize: {e}"))?;

    for attempt in 0..2 {
        let mut tmp =
            tempfile::NamedTempFile::new_in(dir).map_err(|e| format!("temp create: {e}"))?;
        tmp.write_all(&body)
            .map_err(|e| format!("temp write: {e}"))?;
        // Atomic for readers is not the same as durable. Without this, a crash
        // just after the rename can leave a zero-length file where live account
        // state used to be.
        tmp.as_file()
            .sync_all()
            .map_err(|e| format!("temp fsync: {e}"))?;

        // The CLI stores this 0600. A rename carries the TEMP file's mode, so
        // set it before the rename — a window at 0644 would expose OAuth
        // account state to other local users.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tmp.as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("temp chmod: {e}"))?;
        }

        if let Ok(meta) = std::fs::metadata(target) {
            if meta.len() as usize != expected_len {
                return Err(format!(
                    "config changed under us ({} -> {} bytes); not clobbering",
                    expected_len,
                    meta.len()
                ));
            }
        }

        match tmp.persist(target) {
            Ok(_) => return Ok(body.len()),
            // A live CLI session or an AV scanner holding the target open shows
            // up as a sharing violation on Windows. One short backoff converts
            // most of those into a success instead of a trust dialog.
            Err(e) if attempt == 0 => {
                debug!(error = %e, "workspace trust: rename failed, retrying once");
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => return Err(format!("rename: {e}")),
        }
    }
    unreachable!("loop returns on both the success and the final-failure arm")
}

/// Pre-trust `working_dir` in the account configs named by `targets`.
///
/// Best-effort and never fatal — callers ignore the result and spawn either
/// way. A `working_dir` that is relative or does not resolve is skipped
/// outright rather than turned into a key no CLI will read.
pub fn ensure_workspace_trusted(working_dir: &str, targets: TrustTargets<'_>) {
    if working_dir.trim().is_empty() {
        return;
    }
    let Some(key) = project_key(Path::new(working_dir)) else {
        debug!(
            dir = %working_dir,
            "workspace trust: skipped (path is relative or did not resolve)"
        );
        return;
    };

    match targets {
        TrustTargets::Account(Some(dir)) => report(&Path::new(dir).join(CONFIG_FILE), &key),
        TrustTargets::Account(None) => {
            // No account resolved: the child inherits the ambient default, so
            // that is the config it will actually read.
            if let Some(home) = dirs::home_dir() {
                report(&home.join(CONFIG_FILE), &key);
            }
        }
        TrustTargets::EveryKnownAccount => {
            // Read the roster file directly. `settings::get_claude_config_dirs`
            // routes through the full settings load, which is a WRITER by side
            // effect (it can rewrite `claude-accounts.json`, mint a
            // `local_user_id` and reach the OS keyring) — far too much to put
            // on the terminal-creation path.
            let dirs_list = crate::claude_accounts::load()
                .map(|r| r.claude_config_dirs)
                .unwrap_or_default();
            for dir in &dirs_list {
                report(&Path::new(dir).join(CONFIG_FILE), &key);
            }
            if let Some(home) = dirs::home_dir() {
                report(&home.join(CONFIG_FILE), &key);
            }
        }
    }
}

/// Run one pre-trust and log the non-steady-state outcomes. `Trusted` is logged
/// inside [`ensure_trusted_in`] at `info!`, because it is the only record that
/// we rewrote a live, credential-bearing account file.
fn report(config_file: &Path, key: &str) {
    match ensure_trusted_in(config_file, key) {
        TrustOutcome::AlreadyTrusted | TrustOutcome::Trusted => {}
        TrustOutcome::Skipped(reason) => {
            debug!(
                config = %config_file.display(),
                project = %key,
                reason,
                "workspace trust: skipped"
            );
        }
        TrustOutcome::Failed(e) => {
            warn!(
                config = %config_file.display(),
                project = %key,
                error = %e,
                "workspace trust: could not pre-accept; the session may face the trust dialog"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        std::fs::write(path, body).unwrap();
    }

    fn trust_flag(path: &Path, key: &str) -> Option<bool> {
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        doc.get(PROJECTS_KEY)?.get(key)?.get(TRUST_FLAG)?.as_bool()
    }

    /// The core promise: an untrusted project becomes trusted.
    #[test]
    fn sets_the_flag_for_a_new_project() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join(CONFIG_FILE);
        write(&cfg, r#"{"projects":{}}"#);

        assert_eq!(ensure_trusted_in(&cfg, "D:/w/repo"), TrustOutcome::Trusted);
        assert_eq!(trust_flag(&cfg, "D:/w/repo"), Some(true));
    }

    /// An entry that exists but carries the flag as `false` — exactly the shape
    /// observed for the repo that raised the dialog on the operator box — must
    /// be upgraded, not mistaken for already-trusted.
    #[test]
    fn upgrades_an_entry_whose_flag_is_false() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join(CONFIG_FILE);
        write(
            &cfg,
            r#"{"projects":{"D:/w/repo":{"hasTrustDialogAccepted":false,"allowedTools":[]}}}"#,
        );

        assert_eq!(ensure_trusted_in(&cfg, "D:/w/repo"), TrustOutcome::Trusted);
        assert_eq!(trust_flag(&cfg, "D:/w/repo"), Some(true));
    }

    /// Idempotence is what keeps writes rare against a live file — the second
    /// call must not rewrite it.
    #[test]
    fn already_trusted_does_not_rewrite() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join(CONFIG_FILE);
        write(
            &cfg,
            r#"{"projects":{"D:/w/repo":{"hasTrustDialogAccepted":true}}}"#,
        );
        let before = std::fs::read_to_string(&cfg).unwrap();

        assert_eq!(
            ensure_trusted_in(&cfg, "D:/w/repo"),
            TrustOutcome::AlreadyTrusted
        );
        assert_eq!(
            std::fs::read_to_string(&cfg).unwrap(),
            before,
            "the file must be byte-identical when nothing needed changing"
        );
    }

    /// Every sibling VALUE survives. Ordering deliberately is not asserted —
    /// the rewrite alphabetizes, which the module docs state outright.
    #[test]
    fn preserves_unrelated_account_state() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join(CONFIG_FILE);
        write(
            &cfg,
            r#"{"oauthAccount":{"emailAddress":"a@b.c"},"numStartups":41,
                "mcpServers":{"coord":{"url":"http://x"}},
                "projects":{"D:/w/other":{"hasTrustDialogAccepted":true,"history":[1,2]}}}"#,
        );

        assert_eq!(ensure_trusted_in(&cfg, "D:/w/repo"), TrustOutcome::Trusted);

        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
        assert_eq!(doc["oauthAccount"]["emailAddress"], "a@b.c");
        assert_eq!(doc["numStartups"], 41);
        assert_eq!(doc["mcpServers"]["coord"]["url"], "http://x");
        assert_eq!(trust_flag(&cfg, "D:/w/other"), Some(true));
        assert_eq!(doc[PROJECTS_KEY]["D:/w/other"]["history"][1], 2);
        assert_eq!(trust_flag(&cfg, "D:/w/repo"), Some(true));
    }

    /// A config we cannot parse is left strictly alone — we would otherwise
    /// replace a concurrently-half-written file with a "repaired" one that has
    /// lost the account's state.
    #[test]
    fn refuses_to_rewrite_an_unparseable_config() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join(CONFIG_FILE);
        write(&cfg, "{ this is not json");

        assert_eq!(
            ensure_trusted_in(&cfg, "D:/w/repo"),
            TrustOutcome::Skipped("config did not parse")
        );
        assert_eq!(
            std::fs::read_to_string(&cfg).unwrap(),
            "{ this is not json",
            "the original bytes must survive"
        );
    }

    /// An account that has never run gets no invented config file.
    #[test]
    fn missing_config_is_skipped_not_created() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join(CONFIG_FILE);

        assert_eq!(
            ensure_trusted_in(&cfg, "D:/w/repo"),
            TrustOutcome::Skipped("no config file")
        );
        assert!(!cfg.exists(), "must not create the file");
    }

    /// A config with no `projects` map at all still gets one.
    #[test]
    fn creates_the_projects_map_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join(CONFIG_FILE);
        write(&cfg, r#"{"numStartups":1}"#);

        assert_eq!(ensure_trusted_in(&cfg, "D:/w/repo"), TrustOutcome::Trusted);
        assert_eq!(trust_flag(&cfg, "D:/w/repo"), Some(true));
    }

    /// The key is the GIT ROOT, not the cwd — the property that makes the entry
    /// the one the CLI actually looks up.
    #[test]
    fn key_is_the_enclosing_git_root_not_the_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let nested = repo.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir(repo.join(".git")).unwrap();

        assert_eq!(project_key(&nested), project_key(&repo));
        let key = project_key(&nested).unwrap();
        assert!(key.ends_with("/repo"), "got {key}");
    }

    /// A linked worktree marks its root with a `.git` FILE, and is therefore its
    /// own trust key — never covered by the parent repo's entry. This is the
    /// case that makes a one-time backfill insufficient.
    #[test]
    fn a_linked_worktree_is_its_own_key() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let wt = repo.join("agent-worktrees").join("abc");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::create_dir(repo.join(".git")).unwrap();
        write(&wt.join(".git"), "gitdir: ../../.git/worktrees/abc");

        assert_ne!(project_key(&wt), project_key(&repo));
        let key = project_key(&wt).unwrap();
        assert!(key.ends_with("/abc"), "got {key}");
    }

    /// Outside any repository the key is the directory itself.
    #[test]
    fn key_falls_back_to_the_directory_outside_a_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let plain = tmp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();

        assert!(project_key(&plain).unwrap().ends_with("/plain"));
    }

    /// A relative path would resolve against the RUNNER's cwd, and an absent
    /// one cannot resolve at all. Both are refused rather than guessed, so we
    /// never write a key no CLI will look up.
    #[test]
    fn relative_or_missing_paths_yield_no_key() {
        assert_eq!(project_key(Path::new("some/relative/dir")), None);
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(project_key(&tmp.path().join("does-not-exist")), None);
    }

    /// Keys are forward-slashed with no verbatim prefix. The UNC form is the
    /// one that silently produced a `UNC/server/...` key that the CLI could
    /// never match.
    #[test]
    fn keys_strip_both_verbatim_prefixes() {
        assert_eq!(
            to_key_string(Path::new(r"\\?\D:\qontinui-root\ui-bridge")),
            "D:/qontinui-root/ui-bridge"
        );
        assert_eq!(
            to_key_string(Path::new(r"\\?\UNC\server\share\dir")),
            "//server/share/dir"
        );
    }

    /// The stale-document guard: if the file changed between our read and our
    /// rename, we abort rather than clobber the other writer's document.
    #[test]
    fn refuses_to_clobber_a_config_that_changed_under_us() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join(CONFIG_FILE);
        write(&cfg, r#"{"projects":{}}"#);
        let doc: serde_json::Value = serde_json::from_str(r#"{"projects":{}}"#).unwrap();

        // Claim the file was longer when we read it than it is now.
        let err = write_atomically(&cfg, &doc, 9_999).unwrap_err();
        assert!(err.contains("changed under us"), "got {err}");
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), r#"{"projects":{}}"#);
    }
}
