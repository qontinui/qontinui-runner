//! Shim argv **classification** — the single Rust source of truth for which
//! verbs are install-shaped per package manager, and the parse of an
//! install-shaped argv into `(packages, dev, is_lockfile_sync)`.
//!
//! This logic is mirrored by the materialized shim scripts
//! ([`super::shim_materializer`] substitutes the per-PM install-verb table into
//! the bash/`.cmd` payloads from [`install_verbs`]). The Rust functions here are
//! what the unit tests assert against, so the script and the Rust stay
//! lock-stepped on the SAME verb table. The script re-implements the same
//! classification in shell; this module is the canonical spec it must match.

use super::types::{PackageManager, PackageSpecInput};

/// The install-shaped verbs for `pm`. The FIRST non-flag argv token is the
/// "verb"; if it is in this table the invocation is install-shaped and gets the
/// declare→observe→verify straddle. Everything else (`run`, `build`, `test`,
/// `ls`, `list`, `exec`, …) is a zero-overhead passthrough.
///
/// Conservative by design (plan §3/A): a verb that merely *contains* "install"
/// (`install-update`, `install-test`) is NOT in the table, so it does not
/// false-positive. Phase 2 wires the shims for all seven tools (npm + pnpm,
/// yarn, npx, cargo, pip/pip3); this one table is the source of truth they all
/// share.
pub fn install_verbs(pm: PackageManager) -> &'static [&'static str] {
    match pm {
        // npm: `install`, its alias `i`, and `add`. (`ci` is a clean
        // lockfile-driven install — install-shaped, lockfile-sync class.)
        PackageManager::Npm => &["install", "i", "add", "ci"],
        // pnpm: `add`, `install`, alias `i`.
        PackageManager::Pnpm => &["add", "install", "i"],
        // yarn classic: `add`, and a bare `yarn install`.
        PackageManager::Yarn => &["add", "install"],
        // cargo: `add` (cargo-edit, now built in) is a manifest dependency add;
        // `update` (no args) is a lockfile reconciliation (A6). `cargo install`
        // is a GLOBAL binary install, NOT a project dependency — excluded; and
        // `install-update` (cargo-update plugin) merely *contains* "install" —
        // also excluded.
        PackageManager::Cargo => &["add", "update"],
        // pip / pip3 (pip3 maps to this PM): `install`.
        PackageManager::Pip => &["install"],
        // poetry: `add`, and `install` (lockfile sync).
        PackageManager::Poetry => &["add", "install"],
    }
}

/// The verbs that, even when install-shaped, are a **lockfile sync** (no new
/// package args expected) — `npm install`/`npm ci`, `pnpm install`,
/// `yarn install`, `poetry install`. Per A6 these are never gated. Note a BARE
/// `install` with zero package tokens is ALSO lockfile-sync regardless of this
/// table; this set captures verbs that are lockfile-sync by their very name
/// (e.g. `npm ci`) even if tokens follow.
pub fn lockfile_sync_verbs(pm: PackageManager) -> &'static [&'static str] {
    match pm {
        PackageManager::Npm => &["ci"],
        // `cargo update` (lockfile reconciliation, A6) is lockfile-sync by name —
        // never gated even if crate-name args follow (`cargo update serde`).
        PackageManager::Cargo => &["update"],
        _ => &[],
    }
}

/// A materialized shim **tool**: the seven program names the runner shadows on
/// the agent terminal's PATH (plan §4 Phase 2). A tool is NOT 1:1 with
/// [`PackageManager`] — `npx` is an execution tool that wires to `npm`, and
/// `pip3` wires to `pip`. This enum is the single source of truth for which
/// executables get a shim, what wire `package_manager` each reports to the
/// producer (which `pm::parse_token`s only npm/yarn/pnpm/cargo/pip/poetry), and
/// which classification policy each uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShimTool {
    Npm,
    Pnpm,
    Yarn,
    /// EXECUTION tool — `npx <pkg>` fetches into the npm cache + runs. Wires to
    /// `npm`. Install-shaped (declare+observe with the package name) but NEVER
    /// gate-blocked (see [`ShimTool::never_gate`]): npx is execution-shaped, a
    /// false block of a tool an agent is trying to *run* is far worse than a
    /// missed declaration, so npx is observe-only by policy even in gate mode.
    Npx,
    Cargo,
    Pip,
    /// `pip3` — same install semantics as `pip`; wires to `pip`.
    Pip3,
}

/// The seven shim tools the materializer writes per terminal (plan §4 Phase 2).
pub const SHIM_TOOLS: &[ShimTool] = &[
    ShimTool::Npm,
    ShimTool::Pnpm,
    ShimTool::Yarn,
    ShimTool::Npx,
    ShimTool::Cargo,
    ShimTool::Pip,
    ShimTool::Pip3,
];

impl ShimTool {
    /// The executable name the shim shadows on PATH (`npx`, `pip3`, …).
    pub fn program(self) -> &'static str {
        match self {
            ShimTool::Npm => "npm",
            ShimTool::Pnpm => "pnpm",
            ShimTool::Yarn => "yarn",
            ShimTool::Npx => "npx",
            ShimTool::Cargo => "cargo",
            ShimTool::Pip => "pip",
            ShimTool::Pip3 => "pip3",
        }
    }

    /// The wire `package_manager` string this tool reports to the producer's
    /// `POST /install-effects/run` (`pm::parse_token` accepts only the coord
    /// enum tokens, so `npx`→`npm` and `pip3`→`pip`).
    pub fn wire_pm(self) -> PackageManager {
        match self {
            ShimTool::Npm | ShimTool::Npx => PackageManager::Npm,
            ShimTool::Pnpm => PackageManager::Pnpm,
            ShimTool::Yarn => PackageManager::Yarn,
            ShimTool::Cargo => PackageManager::Cargo,
            ShimTool::Pip | ShimTool::Pip3 => PackageManager::Pip,
        }
    }

    /// Whether this tool is **never** gate-blocked even in gate mode (Phase 3).
    /// Only `npx` (execution-shaped — see the variant doc). All others honor the
    /// gate. (Lockfile-sync invocations of the others are independently never
    /// gated via the `lockfile_sync` flag on [`Classification::Install`].)
    pub fn never_gate(self) -> bool {
        matches!(self, ShimTool::Npx)
    }

    /// Resolve a tool name (as the materializer/seam knows it) to a [`ShimTool`].
    pub fn from_program(name: &str) -> Option<ShimTool> {
        SHIM_TOOLS.iter().copied().find(|t| t.program() == name)
    }
}

/// The outcome of classifying a shim argv (the program name is NOT included —
/// `args` is everything after the tool name, exactly what the shim sees in
/// `"$@"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classification {
    /// Not an install (non-install verb, or no verb). The shim execs the real
    /// tool immediately with zero runner contact.
    Passthrough,
    /// An install-shaped invocation. `packages` is the parsed spec set (empty ⇒
    /// lockfile sync), `dev` is whether a dev/save-dev flag was present,
    /// `lockfile_sync` is true when this must NEVER be gated (A6): either the
    /// verb is lockfile-sync by name (`ci`) OR there were zero package tokens.
    Install {
        verb: String,
        packages: Vec<PackageSpecInput>,
        dev: bool,
        lockfile_sync: bool,
    },
}

/// Classify a shim argv for `pm`. Pure + total. Mirrors the shell logic in the
/// materialized script (same verb table, same flag stripping, same `name@req`
/// split). The recursion guard is handled by the shim/seam, not here.
pub fn classify(pm: PackageManager, args: &[String]) -> Classification {
    // First non-flag token is the verb.
    let verb = args.iter().find(|a| !a.starts_with('-'));
    let verb = match verb {
        Some(v) => v.clone(),
        None => return Classification::Passthrough,
    };

    if !install_verbs(pm).contains(&verb.as_str()) {
        return Classification::Passthrough;
    }

    // Parse package tokens + dev flag from everything AFTER the verb.
    let mut packages = Vec::new();
    let mut dev = false;
    let mut seen_verb = false;
    for a in args {
        if !seen_verb {
            if a == &verb {
                seen_verb = true;
            }
            continue;
        }
        if is_dev_flag(a) {
            dev = true;
            continue;
        }
        if a.starts_with('-') {
            // Any other flag is ignored; we do NOT consume a following value as
            // a package (the shim's bare-token scan never treats a flag's value
            // as a package because real install flags that take values are
            // rare and harmless to skip — coord degrades to lockfile-sync).
            continue;
        }
        packages.push(parse_spec(a));
    }

    let verb_is_lockfile_sync = lockfile_sync_verbs(pm).contains(&verb.as_str());
    let lockfile_sync = verb_is_lockfile_sync || packages.is_empty();

    Classification::Install {
        verb,
        packages,
        dev,
        lockfile_sync,
    }
}

/// Classify a shim argv for a [`ShimTool`] (plan §4 Phase 2). This is the
/// tool-aware entry point the materialized scripts mirror; it routes to the
/// right per-tool parse:
/// - `npm`/`pnpm`/`yarn`/`cargo` → the verb-table [`classify`] on the tool's
///   `wire_pm` (cargo `add <crate>@<ver>` version split + `--dev`/`--build`).
/// - `npx` → [`classify_npx`] (execution-shaped; first non-flag token is the
///   package; `-p <pkg>` adds; `-y`/`--yes` ignored; `--help` ⇒ passthrough).
/// - `pip`/`pip3` → [`classify_pip`] (requirement specifiers, `-r`→lockfile-sync,
///   `-e .` editable-local ⇒ passthrough WITHOUT declare, `-U` is still an
///   install of the named packages).
///
/// Pure + total. The recursion guard is handled by the shim/seam, not here.
pub fn classify_tool(tool: ShimTool, args: &[String]) -> Classification {
    match tool {
        ShimTool::Npx => classify_npx(args),
        ShimTool::Pip | ShimTool::Pip3 => classify_pip(args),
        // npm / pnpm / yarn / cargo share the verb-table parse on the wire PM.
        ShimTool::Npm | ShimTool::Pnpm | ShimTool::Yarn | ShimTool::Cargo => {
            classify(tool.wire_pm(), args)
        }
    }
}

/// Classify an `npx` argv (plan §4 Phase 2). npx is an EXECUTION tool: `npx
/// <pkg>` fetches `<pkg>` into the npm cache and runs it — there is no install
/// verb, the first non-flag token IS the package to fetch+run. Policy: declare
/// it as an install-shaped event (so the fetch is observed) but it is NEVER
/// gate-blocked ([`ShimTool::never_gate`]). `-p`/`--package <pkg>` explicitly
/// names a package to fetch (collected). `-y`/`--yes` and other flags are
/// ignored. A help/version probe (`npx --help`, `npx -h`, `npx --version`) with
/// no package token ⇒ passthrough (nothing to fetch).
pub fn classify_npx(args: &[String]) -> Classification {
    // Help/version probes never fetch a package.
    if args.iter().any(|a| {
        matches!(
            a.as_str(),
            "--help" | "-h" | "--version" | "-v" | "--no-install"
        )
    }) {
        // Only bail to passthrough when there's ALSO no package — a real
        // `npx --no-install jest` still names a tool; but `npx --help` alone is
        // a probe. Conservative: if any of these probe flags are present AND no
        // explicit `-p`/positional package, passthrough.
        let has_pkg = npx_first_package(args).is_some()
            || args
                .windows(2)
                .any(|w| matches!(w[0].as_str(), "-p" | "--package"));
        if !has_pkg {
            return Classification::Passthrough;
        }
    }

    let mut packages = Vec::new();
    // `-p <pkg>` / `--package <pkg>` explicit packages.
    let mut i = 0;
    while i < args.len() {
        if matches!(args[i].as_str(), "-p" | "--package") {
            if let Some(val) = args.get(i + 1) {
                if !val.starts_with('-') {
                    packages.push(parse_spec(val));
                    i += 2;
                    continue;
                }
            }
        }
        i += 1;
    }
    // The first bare (non-flag, not a `-p` value) token is the package to run.
    if packages.is_empty() {
        if let Some(pkg) = npx_first_package(args) {
            packages.push(parse_spec(&pkg));
        }
    }

    if packages.is_empty() {
        // `npx` with no resolvable package (e.g. only flags) — nothing to fetch.
        return Classification::Passthrough;
    }

    Classification::Install {
        verb: "npx".to_string(),
        packages,
        dev: false,
        // npx never produces a dependency manifest entry; it is observe-only by
        // policy (the `never_gate` tool flag is the real gate-bypass — this flag
        // keeps the lockfile-sync UX consistent: an npx run is never "blocked").
        lockfile_sync: true,
    }
}

/// The first bare positional that is the npx target package — skipping leading
/// flags and the value consumed by a `-p`/`--package`/`-c`/`--call` option.
fn npx_first_package(args: &[String]) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if matches!(a.as_str(), "-p" | "--package" | "-c" | "--call") {
            // Skip this flag AND its value.
            i += 2;
            continue;
        }
        if a.starts_with('-') {
            i += 1;
            continue;
        }
        return Some(a.clone());
    }
    None
}

/// Classify a `pip`/`pip3` argv (plan §4 Phase 2). Only `pip install …` is
/// install-shaped (`download`, `list`, `show`, `freeze`, … passthrough). Within
/// `install`:
/// - `-r <file>` / `--requirement <file>` ⇒ lockfile-sync class (declare with
///   empty packages, A6 never-gate); the file's contents are not parsed here.
/// - `-e <path>` / `--editable <path>` ⇒ an editable LOCAL install, NOT a
///   registry install ⇒ [`Classification::Passthrough`] (no declare). A mixed
///   `pip install -e . requests` still declares `requests` (the editable target
///   is dropped, the registry package observed).
/// - `-U` / `--upgrade` is a flag, not a package — the named packages are still
///   installed (and observed).
/// - requirement specifiers split on the version comparator
///   (`pkg==1.2`, `pkg>=1.0`, `pkg~=2.0`) into `version_req`.
pub fn classify_pip(args: &[String]) -> Classification {
    let verb = args.iter().find(|a| !a.starts_with('-'));
    let verb = match verb {
        Some(v) => v.clone(),
        None => return Classification::Passthrough,
    };
    if verb != "install" {
        return Classification::Passthrough;
    }

    let mut packages = Vec::new();
    let mut requirement_file = false;
    let mut seen_verb = false;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if !seen_verb {
            if a == &verb {
                seen_verb = true;
            }
            i += 1;
            continue;
        }
        match a.as_str() {
            "-r" | "--requirement" => {
                requirement_file = true;
                i += 2; // skip the file path value
                continue;
            }
            "-e" | "--editable" => {
                // Editable local install — skip the path value, declare nothing
                // for it (handled by the passthrough decision below).
                i += 2;
                continue;
            }
            // Options that take a value we must NOT treat as a package.
            "-c" | "--constraint" | "-i" | "--index-url" | "--extra-index-url" | "--find-links"
            | "-f" | "--target" | "-t" | "--prefix" | "--root" | "--python-version"
            | "--platform" | "--abi" | "--implementation" => {
                i += 2;
                continue;
            }
            _ if a.starts_with('-') => {
                // A bare flag (-U/--upgrade/--user/--no-deps/--pre/…) — ignored.
                i += 1;
                continue;
            }
            _ => {
                packages.push(parse_pip_spec(a));
                i += 1;
            }
        }
    }

    // `-r requirements.txt` with no extra registry packages ⇒ lockfile-sync.
    if requirement_file && packages.is_empty() {
        return Classification::Install {
            verb,
            packages: vec![],
            dev: false,
            lockfile_sync: true,
        };
    }

    // Editable-only (`pip install -e .`) with no registry packages and no `-r`
    // ⇒ NOT a registry install ⇒ passthrough (no declare).
    if packages.is_empty() && !requirement_file {
        return Classification::Passthrough;
    }

    let lockfile_sync = packages.is_empty();
    Classification::Install {
        verb,
        packages,
        dev: false, // pip has no save-dev concept
        lockfile_sync,
    }
}

/// Parse a pip requirement specifier `name[<op><version>]` into a
/// [`PackageSpecInput`]. The version split is on the FIRST PEP-508 comparator
/// (`==`, `>=`, `<=`, `~=`, `!=`, `>`, `<`, `===`) — NOT on `@` (which in pip is
/// a PEP-508 URL/direct-reference, left attached to the name conservatively).
pub fn parse_pip_spec(token: &str) -> PackageSpecInput {
    // Find the earliest comparator position.
    const OPS: &[&str] = &["===", "==", ">=", "<=", "~=", "!=", ">", "<"];
    let mut best: Option<usize> = None;
    for op in OPS {
        if let Some(pos) = token.find(op) {
            best = Some(best.map_or(pos, |b| b.min(pos)));
        }
    }
    match best {
        Some(pos) if pos > 0 => PackageSpecInput {
            name: token[..pos].to_string(),
            version_req: Some(token[pos..].to_string()),
        },
        _ => PackageSpecInput {
            name: token.to_string(),
            version_req: None,
        },
    }
}

/// True iff `flag` requests a dev/development dependency. npm/pnpm use
/// `-D`/`--save-dev`; yarn classic uses `--dev`; cargo `add` uses
/// `--dev`/`--build` (dev-dependencies / build-dependencies — both non-runtime,
/// folded to `dev:true`).
pub fn is_dev_flag(flag: &str) -> bool {
    matches!(
        flag,
        "-D" | "--save-dev" | "--dev" | "--save-development" | "--build"
    )
}

/// Parse a `name[@version_req]` token into a [`PackageSpecInput`], honoring
/// npm scoped names (`@scope/pkg`, `@scope/pkg@^1.2`). The split is on the LAST
/// `@` that is not the scope-leading `@` at position 0.
pub fn parse_spec(token: &str) -> PackageSpecInput {
    if let Some(rest) = token.strip_prefix('@') {
        // Scoped: @scope/name[@ver]
        match rest.split_once('/') {
            Some((scope, name_ver)) => match name_ver.rsplit_once('@') {
                Some((name, ver)) if !ver.is_empty() => PackageSpecInput {
                    name: format!("@{scope}/{name}"),
                    version_req: Some(ver.to_string()),
                },
                _ => PackageSpecInput {
                    name: format!("@{scope}/{name_ver}"),
                    version_req: None,
                },
            },
            // A bare `@something` with no slash: treat the whole thing as a name.
            None => PackageSpecInput {
                name: token.to_string(),
                version_req: None,
            },
        }
    } else {
        match token.rsplit_once('@') {
            Some((name, ver)) if !name.is_empty() && !ver.is_empty() => PackageSpecInput {
                name: name.to_string(),
                version_req: Some(ver.to_string()),
            },
            _ => PackageSpecInput {
                name: token.to_string(),
                version_req: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    fn pkgs(c: &Classification) -> Vec<(String, Option<String>)> {
        match c {
            Classification::Install { packages, .. } => packages
                .iter()
                .map(|p| (p.name.clone(), p.version_req.clone()))
                .collect(),
            _ => panic!("not an install: {c:?}"),
        }
    }

    #[test]
    fn npm_install_with_package_is_install() {
        let c = classify(PackageManager::Npm, &a(&["install", "left-pad"]));
        assert!(matches!(c, Classification::Install { .. }));
        assert_eq!(pkgs(&c), vec![("left-pad".to_string(), None)]);
        if let Classification::Install { lockfile_sync, .. } = c {
            assert!(!lockfile_sync, "a packaged install is not lockfile-sync");
        }
    }

    #[test]
    fn npm_i_alias_is_install() {
        let c = classify(PackageManager::Npm, &a(&["i", "react"]));
        assert_eq!(pkgs(&c), vec![("react".to_string(), None)]);
    }

    #[test]
    fn npm_add_alias_is_install() {
        let c = classify(PackageManager::Npm, &a(&["add", "lodash"]));
        assert_eq!(pkgs(&c), vec![("lodash".to_string(), None)]);
    }

    #[test]
    fn bare_npm_install_is_lockfile_sync_never_gated() {
        let c = classify(PackageManager::Npm, &a(&["install"]));
        match c {
            Classification::Install {
                packages,
                lockfile_sync,
                ..
            } => {
                assert!(packages.is_empty());
                assert!(lockfile_sync, "bare install = lockfile sync (A6)");
            }
            _ => panic!("bare install must classify as Install (lockfile-sync)"),
        }
    }

    #[test]
    fn npm_ci_is_lockfile_sync_even_with_no_args() {
        let c = classify(PackageManager::Npm, &a(&["ci"]));
        match c {
            Classification::Install { lockfile_sync, .. } => {
                assert!(lockfile_sync, "npm ci is lockfile-sync by name (A6)");
            }
            _ => panic!("npm ci must be install-shaped"),
        }
    }

    #[test]
    fn non_install_verbs_passthrough() {
        for v in [
            "run", "build", "test", "ls", "list", "exec", "publish", "link",
        ] {
            assert_eq!(
                classify(PackageManager::Npm, &a(&[v, "build"])),
                Classification::Passthrough,
                "verb {v} must passthrough"
            );
        }
    }

    #[test]
    fn lookalike_verbs_do_not_false_positive() {
        // Verbs that merely CONTAIN "install" must NOT match (zero-FP, the
        // soak gate for Phase 3).
        for v in ["install-test", "install-ci-test", "uninstall", "reinstall"] {
            assert_eq!(
                classify(PackageManager::Npm, &a(&[v])),
                Classification::Passthrough,
                "look-alike verb {v} must passthrough"
            );
        }
    }

    #[test]
    fn no_verb_passthrough() {
        assert_eq!(
            classify(PackageManager::Npm, &a(&[])),
            Classification::Passthrough
        );
        // Only flags, no verb.
        assert_eq!(
            classify(PackageManager::Npm, &a(&["--version"])),
            Classification::Passthrough
        );
    }

    #[test]
    fn save_dev_long_flag_sets_dev() {
        let c = classify(PackageManager::Npm, &a(&["install", "jest", "--save-dev"]));
        match c {
            Classification::Install { dev, packages, .. } => {
                assert!(dev);
                assert_eq!(packages.len(), 1);
                assert_eq!(packages[0].name, "jest");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn dash_d_short_flag_sets_dev() {
        let c = classify(PackageManager::Npm, &a(&["i", "-D", "typescript"]));
        match c {
            Classification::Install { dev, packages, .. } => {
                assert!(dev);
                assert_eq!(packages[0].name, "typescript");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn other_flags_are_stripped_not_treated_as_packages() {
        let c = classify(
            PackageManager::Npm,
            &a(&["install", "--no-audit", "left-pad", "--silent"]),
        );
        assert_eq!(pkgs(&c), vec![("left-pad".to_string(), None)]);
    }

    #[test]
    fn version_req_split_on_at() {
        let c = classify(PackageManager::Npm, &a(&["install", "left-pad@1.3.0"]));
        assert_eq!(
            pkgs(&c),
            vec![("left-pad".to_string(), Some("1.3.0".to_string()))]
        );
    }

    #[test]
    fn scoped_package_without_version() {
        let c = classify(PackageManager::Npm, &a(&["add", "@types/node"]));
        assert_eq!(pkgs(&c), vec![("@types/node".to_string(), None)]);
    }

    #[test]
    fn scoped_package_with_version() {
        let c = classify(PackageManager::Npm, &a(&["add", "@types/node@^20.1.0"]));
        assert_eq!(
            pkgs(&c),
            vec![("@types/node".to_string(), Some("^20.1.0".to_string()))]
        );
    }

    #[test]
    fn multiple_packages() {
        let c = classify(
            PackageManager::Npm,
            &a(&["install", "react", "react-dom@18", "-D"]),
        );
        match &c {
            Classification::Install { dev, .. } => assert!(dev),
            _ => panic!(),
        }
        assert_eq!(
            pkgs(&c),
            vec![
                ("react".to_string(), None),
                ("react-dom".to_string(), Some("18".to_string())),
            ]
        );
    }

    #[test]
    fn cargo_add_is_install_but_cargo_install_is_not() {
        assert!(matches!(
            classify(PackageManager::Cargo, &a(&["add", "serde"])),
            Classification::Install { .. }
        ));
        // `cargo install ripgrep` is a global binary install, NOT a dep.
        assert_eq!(
            classify(PackageManager::Cargo, &a(&["install", "ripgrep"])),
            Classification::Passthrough
        );
    }

    #[test]
    fn pip_install_is_install() {
        let c = classify(PackageManager::Pip, &a(&["install", "requests"]));
        assert_eq!(pkgs(&c), vec![("requests".to_string(), None)]);
    }

    // =======================================================================
    // Phase 2 — per-PM classification (pnpm / yarn / npx / cargo / pip).
    // =======================================================================

    /// Classify a tool's args (everything AFTER the program name) split on
    /// whitespace — the shape the shim sees in `"$@"`.
    fn ct(tool: ShimTool, line: &str) -> Classification {
        let args: Vec<String> = line.split_whitespace().map(|s| s.to_string()).collect();
        classify_tool(tool, &args)
    }

    fn is_install(c: &Classification) -> bool {
        matches!(c, Classification::Install { .. })
    }

    fn is_lockfile_sync(c: &Classification) -> bool {
        matches!(
            c,
            Classification::Install {
                lockfile_sync: true,
                ..
            }
        )
    }

    #[test]
    fn shim_tool_wire_pm_and_program_mapping() {
        assert_eq!(ShimTool::Npx.program(), "npx");
        assert_eq!(ShimTool::Npx.wire_pm(), PackageManager::Npm);
        assert_eq!(ShimTool::Pip3.program(), "pip3");
        assert_eq!(ShimTool::Pip3.wire_pm(), PackageManager::Pip);
        assert_eq!(ShimTool::Cargo.wire_pm(), PackageManager::Cargo);
        assert!(ShimTool::Npx.never_gate(), "npx is never gate-blocked");
        assert!(!ShimTool::Npm.never_gate());
        assert_eq!(ShimTool::from_program("pip3"), Some(ShimTool::Pip3));
        assert_eq!(ShimTool::from_program("rustc"), None);
        assert_eq!(SHIM_TOOLS.len(), 7, "seven shim tools");
    }

    #[test]
    fn pnpm_add_and_install() {
        assert_eq!(
            pkgs(&ct(ShimTool::Pnpm, "add lodash")),
            vec![("lodash".to_string(), None)]
        );
        // bare `pnpm install` = lockfile-sync.
        assert!(is_lockfile_sync(&ct(ShimTool::Pnpm, "install")));
        // `pnpm i react` = install with args.
        assert_eq!(
            pkgs(&ct(ShimTool::Pnpm, "i react")),
            vec![("react".to_string(), None)]
        );
        // `pnpm run dev` passthrough.
        assert_eq!(ct(ShimTool::Pnpm, "run dev"), Classification::Passthrough);
    }

    #[test]
    fn yarn_add_and_bare_install() {
        assert_eq!(
            pkgs(&ct(ShimTool::Yarn, "add chalk")),
            vec![("chalk".to_string(), None)]
        );
        assert!(is_lockfile_sync(&ct(ShimTool::Yarn, "install")));
        // bare `yarn` (no verb) passthrough — lockfile sync is only via `install`.
        assert_eq!(ct(ShimTool::Yarn, ""), Classification::Passthrough);
        assert_eq!(ct(ShimTool::Yarn, "build"), Classification::Passthrough);
    }

    #[test]
    fn npx_runs_a_package_is_install_but_never_gated() {
        let c = ct(ShimTool::Npx, "create-react-app my-app");
        assert_eq!(pkgs(&c), vec![("create-react-app".to_string(), None)]);
        // Always lockfile-sync flagged (never-gated UX).
        assert!(is_lockfile_sync(&c));
        assert!(ShimTool::Npx.never_gate());
    }

    #[test]
    fn npx_explicit_package_flag() {
        let c = ct(ShimTool::Npx, "-p typescript@5 tsc --noEmit");
        assert_eq!(
            pkgs(&c),
            vec![("typescript".to_string(), Some("5".to_string()))]
        );
    }

    #[test]
    fn npx_yes_flag_then_package() {
        let c = ct(ShimTool::Npx, "-y cowsay hello");
        assert_eq!(pkgs(&c), vec![("cowsay".to_string(), None)]);
    }

    #[test]
    fn npx_help_is_passthrough() {
        assert_eq!(ct(ShimTool::Npx, "--help"), Classification::Passthrough);
        assert_eq!(ct(ShimTool::Npx, "-h"), Classification::Passthrough);
        assert_eq!(ct(ShimTool::Npx, "--version"), Classification::Passthrough);
    }

    #[test]
    fn cargo_add_with_version_and_dev() {
        let c = ct(ShimTool::Cargo, "add serde@1.0");
        assert_eq!(
            pkgs(&c),
            vec![("serde".to_string(), Some("1.0".to_string()))]
        );
        match ct(ShimTool::Cargo, "add tokio --dev") {
            Classification::Install { dev, .. } => assert!(dev),
            _ => panic!(),
        }
        match ct(ShimTool::Cargo, "add cc --build") {
            Classification::Install { dev, .. } => assert!(dev, "--build folds to dev"),
            _ => panic!(),
        }
    }

    #[test]
    fn cargo_install_and_install_update_passthrough() {
        // `cargo install ripgrep` = global binary, NOT a dep.
        assert_eq!(
            ct(ShimTool::Cargo, "install ripgrep"),
            Classification::Passthrough
        );
        // `cargo install-update -a` (cargo-update plugin) merely contains
        // "install" — must NOT classify.
        assert_eq!(
            ct(ShimTool::Cargo, "install-update -a"),
            Classification::Passthrough
        );
    }

    #[test]
    fn cargo_update_is_lockfile_sync_never_gated() {
        assert!(is_lockfile_sync(&ct(ShimTool::Cargo, "update")));
        // Even with a crate name, `cargo update serde` is lockfile reconciliation.
        assert!(is_lockfile_sync(&ct(ShimTool::Cargo, "update serde")));
    }

    #[test]
    fn pip_requirement_specifiers() {
        let c = ct(ShimTool::Pip, "install requests==2.31.0");
        assert_eq!(
            pkgs(&c),
            vec![("requests".to_string(), Some("==2.31.0".to_string()))]
        );
        let c = ct(ShimTool::Pip, "install flask>=2.0");
        assert_eq!(
            pkgs(&c),
            vec![("flask".to_string(), Some(">=2.0".to_string()))]
        );
        let c = ct(ShimTool::Pip3, "install numpy~=1.26");
        assert_eq!(
            pkgs(&c),
            vec![("numpy".to_string(), Some("~=1.26".to_string()))]
        );
    }

    #[test]
    fn pip_requirements_file_is_lockfile_sync() {
        assert!(is_lockfile_sync(&ct(
            ShimTool::Pip,
            "install -r requirements.txt"
        )));
        assert!(is_lockfile_sync(&ct(
            ShimTool::Pip,
            "install --requirement dev-requirements.txt"
        )));
    }

    #[test]
    fn pip_editable_local_is_passthrough() {
        // `pip install -e .` is NOT a registry install → no declare.
        assert_eq!(
            ct(ShimTool::Pip, "install -e ."),
            Classification::Passthrough
        );
        assert_eq!(
            ct(ShimTool::Pip, "install --editable ./pkg"),
            Classification::Passthrough
        );
        // Mixed: editable target dropped, the registry package still declared.
        let c = ct(ShimTool::Pip, "install -e . requests");
        assert_eq!(pkgs(&c), vec![("requests".to_string(), None)]);
    }

    #[test]
    fn pip_upgrade_flag_still_installs_named_packages() {
        let c = ct(ShimTool::Pip, "install -U black");
        assert_eq!(pkgs(&c), vec![("black".to_string(), None)]);
        let c = ct(ShimTool::Pip, "install --upgrade pytest mypy");
        assert_eq!(
            pkgs(&c),
            vec![("pytest".to_string(), None), ("mypy".to_string(), None),]
        );
    }

    #[test]
    fn pip_index_url_value_not_treated_as_package() {
        let c = ct(ShimTool::Pip, "install -i https://pypi.org/simple requests");
        assert_eq!(pkgs(&c), vec![("requests".to_string(), None)]);
    }

    // =======================================================================
    // SOAK CORPUS — the Phase-3 gate (plan §4 Phase 2).
    //
    // A green soak (FALSE-POSITIVE rate == 0 AND FALSE-NEGATIVE rate == 0) over
    // this corpus of real-shaped agent command lines is the PRECONDITION for
    // enabling gate mode (Phase 3). Do NOT flip `QONTINUI_INSTALL_INTERCEPT_MODE`
    // to `gate` until this test is green: a false-positive here is a spurious
    // gate/block of a command that was never an install.
    //
    //   * `installs`   — must classify as Install (FALSE-NEGATIVE = 0).
    //   * `lookalikes` — install-shaped tokens that must NOT install (the core
    //                    false-positive risk: `cargo install-update`,
    //                    `npm install-test`, `pip download`, `pip install -h`,
    //                    `npm run install:deps`, …). FALSE-POSITIVE = 0.
    //   * `noninstall` — ordinary non-install verbs per PM. FALSE-POSITIVE = 0.
    // =======================================================================

    /// (tool, args-after-program). `expect_install` is the ground truth.
    struct Case {
        tool: ShimTool,
        line: &'static str,
        expect_install: bool,
    }

    fn soak_corpus() -> Vec<Case> {
        let inst = |tool, line| Case {
            tool,
            line,
            expect_install: true,
        };
        let pass = |tool, line| Case {
            tool,
            line,
            expect_install: false,
        };
        vec![
            // ---- true installs (FALSE-NEGATIVE must be 0) ------------------
            inst(ShimTool::Npm, "install left-pad"),
            inst(ShimTool::Npm, "i react"),
            inst(ShimTool::Npm, "add lodash"),
            inst(ShimTool::Npm, "install"), // lockfile sync
            inst(ShimTool::Npm, "ci"),
            inst(ShimTool::Npm, "install @types/node@^20.1.0"),
            inst(ShimTool::Npm, "install react react-dom@18 -D"),
            inst(ShimTool::Npm, "install --no-audit express --silent"),
            inst(ShimTool::Pnpm, "add zod"),
            inst(ShimTool::Pnpm, "install"),
            inst(ShimTool::Pnpm, "i -D vitest"),
            inst(ShimTool::Pnpm, "add @scope/pkg@1.2.3"),
            inst(ShimTool::Yarn, "add chalk"),
            inst(ShimTool::Yarn, "install"),
            inst(ShimTool::Yarn, "add --dev jest"),
            inst(ShimTool::Npx, "create-react-app my-app"),
            inst(ShimTool::Npx, "-y cowsay moo"),
            inst(ShimTool::Npx, "-p typescript tsc"),
            inst(ShimTool::Npx, "prettier --write ."),
            inst(ShimTool::Cargo, "add serde"),
            inst(ShimTool::Cargo, "add tokio@1 --features full"),
            inst(ShimTool::Cargo, "add anyhow --dev"),
            inst(ShimTool::Cargo, "add cc --build"),
            inst(ShimTool::Cargo, "update"), // lockfile sync
            inst(ShimTool::Cargo, "update serde"),
            inst(ShimTool::Pip, "install requests"),
            inst(ShimTool::Pip, "install requests==2.31.0"),
            inst(ShimTool::Pip, "install flask>=2.0"),
            inst(ShimTool::Pip, "install -U black"),
            inst(ShimTool::Pip, "install --upgrade pytest mypy"),
            inst(ShimTool::Pip, "install -r requirements.txt"), // lockfile sync
            inst(ShimTool::Pip3, "install numpy~=1.26"),
            inst(ShimTool::Pip3, "install -i https://pypi.org/simple httpx"),
            // ---- look-alikes (FALSE-POSITIVE must be 0) --------------------
            pass(ShimTool::Cargo, "install ripgrep"), // global binary
            pass(ShimTool::Cargo, "install-update -a"), // cargo-update plugin
            pass(ShimTool::Cargo, "install-update"),
            pass(ShimTool::Npm, "install-test"), // hyphenated single token
            pass(ShimTool::Npm, "run install:deps"), // run script named install
            pass(ShimTool::Npm, "run build"),
            pass(ShimTool::Yarn, "run add-license"), // script named add-*
            pass(ShimTool::Cargo, "test install"),   // 'install' is a test name
            pass(ShimTool::Pip, "download requests"), // pip download != install
            pass(ShimTool::Pip, "install -h"),       // help: no package → ??? see note
            pass(ShimTool::Pip, "install --help"),
            pass(ShimTool::Npx, "--help"),
            pass(ShimTool::Npx, "-h"),
            pass(ShimTool::Npm, "uninstall left-pad"),
            pass(ShimTool::Npm, "reinstall"),
            pass(ShimTool::Pnpm, "install-test"),
            // ---- non-install verbs per PM (FALSE-POSITIVE must be 0) -------
            pass(ShimTool::Npm, "run build"),
            pass(ShimTool::Npm, "test"),
            pass(ShimTool::Npm, "ls"),
            pass(ShimTool::Npm, "publish"),
            pass(ShimTool::Pnpm, "run dev"),
            pass(ShimTool::Pnpm, "exec eslint"),
            pass(ShimTool::Yarn, "build"),
            pass(ShimTool::Yarn, "test --coverage"),
            pass(ShimTool::Cargo, "test"),
            pass(ShimTool::Cargo, "build --release"),
            pass(ShimTool::Cargo, "fmt"),
            pass(ShimTool::Cargo, "clippy -- -D warnings"),
            pass(ShimTool::Pip, "list"),
            pass(ShimTool::Pip, "freeze"),
            pass(ShimTool::Pip, "show requests"),
            pass(ShimTool::Pip3, "list --outdated"),
            // ---- edge cases ------------------------------------------------
            pass(ShimTool::Pip, "install -e ."), // editable local → no declare
            pass(ShimTool::Pip, "install --editable ./pkg"),
            inst(ShimTool::Pip, "install -e . requests"), // mixed → registry pkg declared
            inst(ShimTool::Npm, "install -- left-pad"),   // `--` separator
            inst(ShimTool::Npm, "install @scope/pkg@^1.2"), // scoped + version
            pass(ShimTool::Npm, ""),                      // empty argv
            pass(ShimTool::Npm, "--version"),             // only flags, no verb
        ]
    }

    #[test]
    fn soak_zero_false_positive_and_zero_false_negative() {
        assert!(
            soak_corpus().len() >= 60,
            "soak corpus must be >=60 entries (got {})",
            soak_corpus().len()
        );
        let mut false_pos = Vec::new();
        let mut false_neg = Vec::new();
        for case in soak_corpus() {
            let args: Vec<String> = case
                .line
                .split_whitespace()
                .map(|s| s.to_string())
                .collect();
            let c = classify_tool(case.tool, &args);
            let got_install = is_install(&c);
            if got_install && !case.expect_install {
                false_pos.push((case.tool, case.line, c.clone()));
            }
            if !got_install && case.expect_install {
                false_neg.push((case.tool, case.line, c.clone()));
            }
        }
        assert!(
            false_pos.is_empty(),
            "FALSE-POSITIVE rate must be 0; spurious installs: {false_pos:?}"
        );
        assert!(
            false_neg.is_empty(),
            "FALSE-NEGATIVE rate must be 0; missed installs: {false_neg:?}"
        );
    }
}
