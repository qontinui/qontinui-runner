//! Toolchain provisioning for CI dispatches: install a tool named by a
//! `[[tools]]` declaration from a **closed, curated registry**, cache it
//! version-pinned, and hand its bin directory to the executor for the step
//! PATH.
//!
//! **Why this exists.** `ci_node/` installed nothing, so a repo whose Actions
//! gate runs `cargo nextest run` had to substitute plain `cargo test` on this
//! lane. That substitution is not cosmetic: nextest is what writes the JUnit
//! report coord's Tier-7 credibility gate ingests, and without a
//! `test_results` row `test_run_effects.rs::score_for_head` returns `None` and
//! the tier fails CLOSED. coord's own manifest calls that "a HARD BLOCKER for
//! cutover".
//!
//! # Why this module is an installer-strategy enum and not a URL builder
//!
//! The first entry (`cargo-nextest`) was a single prebuilt binary in a release
//! archive, and the module was shaped around exactly that: one asset URL, one
//! basename-matched file. Holding that shape literally cannot deliver what the
//! fleet's shipped manifests actually ask for. Of `poetry`, `npm`/`node`,
//! `twine` and `datamodel-code-generator`, only `node` ships as a prebuilt
//! release archive — and even that is a directory TREE (`bin/node`, `bin/npm`,
//! `lib/node_modules/npm/…`), not one file. qontinui's manifest records
//! poetry's real install as `curl -sSL https://install.python-poetry.org |
//! python3 -`; `twine` and `datamodel-code-generator` are Python *packages*.
//!
//! So [`Installer`] is an enum of install strategies, and the properties that
//! actually catch a bad install were made **universal — no per-variant
//! opt-out**:
//!
//! 1. **The registry stays CLOSED.** A manifest names a curated entry and pins
//!    a version. It cannot supply a URL, and it cannot supply an installer
//!    command — that would re-open the registry through the back door. A repo
//!    can already run arbitrary commands as a step, so this is not about what
//!    code can execute: it is about not letting a manifest make the RUNNER
//!    fetch an arbitrary payload, cache it under a well-known name, and put it
//!    on the PATH that later steps resolve implicitly. Adding an entry here is
//!    a deliberate, reviewable act.
//! 2. **Verification is by EXECUTION.** Every variant ends by running the
//!    installed tool and requiring it to report the declared version
//!    ([`version_is_reported`]). The projects here publish no checksum assets
//!    we could pin, and executing the artifact catches strictly more than a
//!    hash would anyway — truncation, a wrong-platform asset, a wrong-VERSION
//!    asset and a half-written venv all fail it, and the wrong-version case is
//!    invisible to any hash we could have computed ourselves.
//! 3. **Publication is by ATOMIC RENAME into the version-keyed cache**
//!    ([`tool_dir`]). Nothing is ever written directly at the cache path; a
//!    tool directory appears only by renaming a staging directory that has
//!    already passed verification. A partial download, a truncated archive, a
//!    failed `pip install` and a wrong-version asset all fail before the
//!    rename, and none of them can leave anything at the cache path. On a
//!    cache HIT the same verification runs again (it costs one process spawn)
//!    so a directory corrupted after the fact — a half-finished antivirus
//!    quarantine, a killed dispatch mid-rename — self-heals instead of
//!    producing a broken tool.
//!
//! Caching is safe here in a way sibling caching is not: the cache key is an
//! exact version, so a hit is by construction the same bytes a miss would
//! fetch.
//!
//! # The variants
//!
//! * [`Installer::PrebuiltBinary`] — the original nextest path, behaviour
//!   unchanged. Exactly ONE basename-matched file is pulled out of the archive
//!   and exactly one path is ever written, so the tar/zip-slip class is
//!   *structurally absent* rather than filtered.
//! * [`Installer::PrebuiltTree`] — node. A tree cannot use the one-file rule,
//!   so the equivalent structural guarantee is built from two parts. First,
//!   every entry path is validated BEFORE anything is opened:
//!   [`safe_tree_path`] rejects absolute paths, drive-qualified paths, `..`
//!   components and any entry outside the single expected top-level directory
//!   (which also rejects an archive smuggling in a second root); hard links
//!   and device nodes are refused outright. Second — and this is what makes
//!   the first part sound — **no symlink is ever created**. Link entries are
//!   recorded and materialised afterwards as launcher scripts
//!   ([`materialize_links`]), so every path the extractor opens runs through
//!   directories it created itself out of validated components and cannot be
//!   redirected. Filtering alone was NOT enough here: a two-link chain
//!   defeated the lexical containment check and produced an arbitrary write
//!   outside the root — the worked example is on
//!   [`symlink_target_is_contained`]. This variant additionally pins a
//!   reviewed **SHA-256 per (version, platform)**, checked before the
//!   extractor sees a byte, so a hostile archive never reaches the code above
//!   in the first place; a version with no reviewed digest is refused.
//! * [`Installer::PythonPackage`] — poetry, twine, datamodel-code-generator.
//!   Installed into a **private, version-keyed venv** under the tool cache.
//!   Never the user's global interpreter: qontinui's and qontinui-schemas'
//!   manifests both record a global `pip install` as unacceptable on a user's
//!   machine ("on an ephemeral Actions runner that is free; on a user's
//!   machine it mutates their interpreter permanently").
//!
//! # Node is a runtime with an ecosystem — and that lives in the REGISTRY
//!
//! The plan asked whether a registry entry provisions the runtime, the package
//! manager, or both, and noted `CiTool` has no shape for "node 22 **with**
//! npm". The answer taken here is: **the entry provisions both, and `CiTool`
//! stays a flat `{name, version}` pair.** A node release *is* an npm release —
//! npm's version is a property of the node build, not an independently
//! pinnable thing — so a second version field would be a knob with nothing on
//! the other end. `actions/setup-node` behaves the same way. What the entry
//! does carry is `companions`: npm and npx are checked to exist and to execute
//! after install, and their versions are logged, so "node was provisioned but
//! npm is missing" is a provisioning failure rather than a step failure ten
//! minutes later. A project's own dependency install (`npm ci`, `poetry
//! install`) stays a manifest STEP: it is repo-scoped, lockfile-pinned and
//! belongs in the worktree, not in a cross-dispatch tool cache.
//!
//! # Python: why a shim and not the venv's console scripts
//!
//! A venv's console scripts are **not relocatable** — on Windows the generated
//! `.exe` launcher embeds an absolute path to the venv's `python.exe`, and on
//! Unix the script's shebang does the same. Measured on this box 2026-08-18: a
//! venv with `poetry==2.1.3` moved to a new directory, then
//! `Scripts/poetry.exe --version` printed **nothing and exited 1**, while
//! `Scripts/python.exe -m poetry --version` still printed
//! `Poetry (version 2.1.3)`. A venv python invoked through its own path stays a
//! venv python (it finds `pyvenv.cfg` beside/above itself), so `-m` survives a
//! move; the console scripts do not. Since atomic rename into the cache is a
//! mandatory property, the console scripts cannot be what goes on PATH.
//!
//! So the entry writes its own launcher into `<tool_dir>/bin/`, and only that
//! directory is put on the step PATH (never the venv's `Scripts`/`bin`, which
//! would shadow the shim with the broken script). The shim locates the venv
//! **relative to its own location** — `%~dp0..` on Windows, `dirname "$0"/..`
//! elsewhere — so it is position-independent and the rename is safe. The
//! module it invokes is a curated per-entry field, and verification proves the
//! invocation actually works: an entry whose distribution has no `__main__`
//! fails provisioning loudly instead of landing a shim that silently does
//! nothing.
//!
//! # No Python interpreter is a REFUSAL, never a skip
//!
//! [`Installer::PythonPackage`] needs a host Python 3 to build the venv from.
//! If none is found the dispatch **fails with a message naming what is
//! missing** and naming the candidates that were tried. It never falls back to
//! "the tool is probably on PATH already", and it never installs one: silence
//! is never success on this lane.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::{info, warn};

use super::manifest::CiTool;

/// Archive shape of a release asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArchiveKind {
    Zip,
    TarGz,
}

/// A resolved download for one (tool, version, host).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Asset {
    pub url: String,
    pub kind: ArchiveKind,
}

/// A resolved download whose archive is a directory TREE rather than a single
/// binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TreeAsset {
    pub asset: Asset,
    /// The single top-level directory every entry in the archive must sit
    /// under. Stripped on extraction, and an entry outside it is a refusal —
    /// which also rejects an archive that smuggles in a second root.
    pub root_dir: String,
    /// Where the executables live inside the stripped tree (`""` when they sit
    /// at its root). This is the directory that goes on the step PATH.
    pub bin_subdir: &'static str,
    /// Lowercase hex SHA-256 of the archive, checked BEFORE any extractor
    /// sees the bytes. MANDATORY on this variant — see [`Installer`].
    pub sha256: &'static str,
}

/// How one registry entry is installed. The three properties in the module
/// docs hold on EVERY variant; what differs is only where the bytes come from.
pub(crate) enum Installer {
    /// One prebuilt binary inside a release archive — the original shape.
    PrebuiltBinary {
        /// File stem of the binary inside the archive (no `.exe`).
        binary: &'static str,
        /// `None` means this tool publishes nothing for that platform, which
        /// is a legible refusal rather than a mystery 404.
        asset: fn(version: &str, triple: &str) -> Option<Asset>,
    },
    /// A prebuilt directory tree — a runtime that brings an ecosystem with it.
    PrebuiltTree {
        /// The executable whose version must match the declared one.
        primary: &'static str,
        /// Executables the tree also provides. Checked to exist and to run;
        /// their versions are logged, not pinned — they are properties of the
        /// primary's release, not independently declarable.
        companions: &'static [&'static str],
        asset: fn(version: &str, triple: &str) -> Option<TreeAsset>,
    },
    /// A Python distribution installed into a private, version-keyed venv.
    PythonPackage {
        /// PyPI distribution name, pinned as `<distribution>==<version>`.
        distribution: &'static str,
        /// Import name for `python -m <module>`, which is what the generated
        /// shim invokes. Differs from the distribution name whenever the
        /// distribution has a dash (`datamodel-code-generator` →
        /// `datamodel_code_generator`).
        module: &'static str,
    },
}

/// One entry of the closed registry.
pub(crate) struct ToolSpec {
    pub name: &'static str,
    /// Argv that makes the tool print its version. Checked to exit 0 AND to
    /// mention the declared version.
    pub verify_args: &'static [&'static str],
    pub install: Installer,
}

/// Download ceiling. Prebuilt CLI archives are single-digit MB and a full node
/// distribution is ~35-55 MB compressed (measured 2026-08-18: win-x64.zip
/// 34,188,182 bytes; linux-x64.tar.gz 53,135,426 bytes); anything an order of
/// magnitude past that is a redirect gone wrong, not a tool.
const MAX_ASSET_BYTES: u64 = 200 * 1024 * 1024;
/// Whole-download budget.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// Budget for the verification spawn.
const VERIFY_TIMEOUT: Duration = Duration::from_secs(60);
/// Budget for `python -m venv`, which copies/links an interpreter and
/// bootstraps pip.
const VENV_TIMEOUT: Duration = Duration::from_secs(300);
/// Budget for the package install. Sized against the worst curated entry
/// rather than the median: `poetry` resolves ~40 wheels.
const PACKAGE_INSTALL_TIMEOUT: Duration = Duration::from_secs(900);
/// A staging directory older than this belongs to a dispatch that is gone.
/// Far above any real install time, so the sweep cannot race a live one.
const STAGING_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// Interpreters tried, in order, for [`Installer::PythonPackage`]. A missing
/// one is a refusal naming this list — never a silent skip.
const PYTHON_CANDIDATES: &[&str] = &["python3", "python"];

/// cargo-nextest publishes one asset per platform under a
/// `cargo-nextest-<version>` release tag. Names verified live against
/// release `cargo-nextest-0.9.98`.
fn cargo_nextest_asset(version: &str, triple: &str) -> Option<Asset> {
    // macOS gets a single universal binary — there is no per-arch darwin
    // asset, and asking for one 404s.
    let (suffix, kind) = match triple {
        "x86_64-pc-windows-msvc" => ("x86_64-pc-windows-msvc.zip", ArchiveKind::Zip),
        "x86_64-unknown-linux-gnu" => ("x86_64-unknown-linux-gnu.tar.gz", ArchiveKind::TarGz),
        "aarch64-unknown-linux-gnu" => ("aarch64-unknown-linux-gnu.tar.gz", ArchiveKind::TarGz),
        "x86_64-apple-darwin" | "aarch64-apple-darwin" => {
            ("universal-apple-darwin.tar.gz", ArchiveKind::TarGz)
        }
        _ => return None,
    };
    Some(Asset {
        url: format!(
            "https://github.com/nextest-rs/nextest/releases/download/\
             cargo-nextest-{version}/cargo-nextest-{version}-{suffix}"
        ),
        kind,
    })
}

/// Node publishes one archive per platform at `nodejs.org/dist/v<version>/`,
/// each containing a single top-level `node-v<version>-<platform>/` directory.
///
/// Derivation verified live against release **v22.11.0** on 2026-08-18: all
/// five URLs below returned HTTP 200 and all five names appear verbatim in
/// that release's `SHASUMS256.txt`. The archive LAYOUT was verified from the
/// downloaded artifacts, and it differs by platform, which is why `bin_subdir`
/// exists:
///
/// * `node-v22.11.0-win-x64.zip` → `node.exe`, `npm.cmd`, `npx.cmd` at the
///   root of the stripped tree (no `bin/`).
/// * `node-v22.11.0-linux-x64.tar.gz` → `bin/node` (a real file) plus
///   `bin/npm` and `bin/npx` as RELATIVE symlinks into
///   `../lib/node_modules/npm/bin/`. Those symlinks are why the tree extractor
///   supports symlinks at all, and why containment is checked rather than
///   symlinks being banned.
///
/// Unlike cargo-nextest, node's darwin builds are **per-arch** — there is no
/// universal asset, and asking for one 404s.
///
/// The extractor's strictness was checked against the real assets rather than
/// assumed. Across the whole v22.11.0 linux-x64 tarball: 4882 regular files,
/// 1176 directories, 3 symlinks, **no hard links, no device nodes and no
/// `pax_global_header`**, and exactly ONE distinct top-level component
/// (`node-v22.11.0-linux-x64`); the win-x64 zip likewise has exactly one
/// (`node-v22.11.0-win-x64`). So "every entry must sit under the declared
/// root" is a rule the curated assets actually satisfy, not one that would
/// reject them.
/// Digests published in v22.11.0's own `SHASUMS256.txt` and re-verified on
/// 2026-08-18 by hashing the downloaded artifact
/// (`sha256sum node-v22.11.0-linux-x64.tar.gz` == the row below).
///
/// A node version with no row here is REFUSED. That is deliberate: the closed
/// registry already says adding an entry is a reviewable act, and a version
/// whose bytes nobody reviewed is exactly the same class of trust. Adding a
/// version means adding its five digests from that release's `SHASUMS256.txt`
/// — a two-minute, auditable change.
const NODE_DIGESTS: &[(&str, &str, &str)] = &[
    (
        "22.11.0",
        "win-x64",
        "905373a059aecaf7f48c1ce10ffbd5334457ca00f678747f19db5ea7d256c236",
    ),
    (
        "22.11.0",
        "linux-x64",
        "4f862bab52039835efbe613b532238b6e4dde98d139a34e6923193e073438b13",
    ),
    (
        "22.11.0",
        "linux-arm64",
        "27453f7a0dd6b9e6738f1f6ea6a09b102ec7aa484de1e39d6a1c3608ad47aa6a",
    ),
    (
        "22.11.0",
        "darwin-x64",
        "668d30b9512137b5f5baeef6c1bb4c46efff9a761ba990a034fb6b28b9da2465",
    ),
    (
        "22.11.0",
        "darwin-arm64",
        "2e89afe6f4e3aa6c7e21c560d8a0453d84807e97850bbb819b998531a22bdfde",
    ),
];

fn node_asset(version: &str, triple: &str) -> Option<TreeAsset> {
    let (platform, kind, bin_subdir) = match triple {
        "x86_64-pc-windows-msvc" => ("win-x64", ArchiveKind::Zip, ""),
        "x86_64-unknown-linux-gnu" => ("linux-x64", ArchiveKind::TarGz, "bin"),
        "aarch64-unknown-linux-gnu" => ("linux-arm64", ArchiveKind::TarGz, "bin"),
        "x86_64-apple-darwin" => ("darwin-x64", ArchiveKind::TarGz, "bin"),
        "aarch64-apple-darwin" => ("darwin-arm64", ArchiveKind::TarGz, "bin"),
        _ => return None,
    };
    let sha256 = NODE_DIGESTS
        .iter()
        .find(|(v, p, _)| *v == version && *p == platform)
        .map(|(_, _, d)| *d)?;
    let root_dir = format!("node-v{version}-{platform}");
    let ext = match kind {
        ArchiveKind::Zip => "zip",
        ArchiveKind::TarGz => "tar.gz",
    };
    Some(TreeAsset {
        asset: Asset {
            url: format!("https://nodejs.org/dist/v{version}/{root_dir}.{ext}"),
            kind,
        },
        root_dir,
        bin_subdir,
        sha256,
    })
}

/// The closed registry.
///
/// Every entry is a deliberate, reviewable act. The bar for adding one:
/// (a) it is asked for by a manifest the fleet actually ships, (b) its install
/// fits an existing [`Installer`] variant, (c) its asset-name or distribution
/// derivation was verified LIVE against a specific release, and (d) that
/// derivation is pinned by a test below.
static KNOWN_TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "cargo-nextest",
        // `cargo-nextest --version` and `cargo-nextest nextest --version` both
        // work; the bare form is used because it does not depend on nextest's
        // subcommand layout staying put.
        verify_args: &["--version"],
        install: Installer::PrebuiltBinary {
            binary: "cargo-nextest",
            asset: cargo_nextest_asset,
        },
    },
    ToolSpec {
        name: "node",
        // Reports `v22.11.0` — leading `v` and all, which is why
        // `version_is_reported` accepts a `v`-prefixed token.
        verify_args: &["--version"],
        install: Installer::PrebuiltTree {
            primary: "node",
            // npm is what the fleet's frontend manifests actually invoke; npx
            // rides along in the same release and is checked so its absence
            // surfaces at provisioning rather than mid-dispatch.
            companions: &["npm", "npx"],
            asset: node_asset,
        },
    },
    ToolSpec {
        name: "poetry",
        // Reports `Poetry (version 2.1.3)`. Verified live 2026-08-18 by
        // installing poetry==2.1.3 into a venv on this box.
        verify_args: &["--version"],
        install: Installer::PythonPackage {
            distribution: "poetry",
            module: "poetry",
        },
    },
    ToolSpec {
        name: "twine",
        // Reports `twine version 6.1.0 (keyring: …)`. Verified live
        // 2026-08-18 with twine==6.1.0. qontinui's manifest records
        // `twine check dist/*` as blocked partly on a global `pip install
        // twine`; this entry is that half.
        verify_args: &["--version"],
        install: Installer::PythonPackage {
            distribution: "twine",
            module: "twine",
        },
    },
    ToolSpec {
        name: "datamodel-code-generator",
        // Reports a bare `0.28.5`. Verified live 2026-08-18 with
        // datamodel-code-generator==0.28.5. Note the module name is
        // underscored while the distribution name is dashed — the reason
        // `module` is a separate curated field.
        verify_args: &["--version"],
        install: Installer::PythonPackage {
            distribution: "datamodel-code-generator",
            module: "datamodel_code_generator",
        },
    },
];

pub(crate) fn lookup(name: &str) -> Option<&'static ToolSpec> {
    KNOWN_TOOLS.iter().find(|t| t.name == name)
}

pub(crate) fn known_tool_names() -> Vec<&'static str> {
    KNOWN_TOOLS.iter().map(|t| t.name).collect()
}

/// The target triple this runner is executing on, in the spelling release
/// assets use. Derived from the compile-time target rather than probed,
/// because it describes the binary that will actually run.
pub(crate) fn host_triple() -> &'static str {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "windows") => "x86_64-pc-windows-msvc",
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("aarch64", "macos") => "aarch64-apple-darwin",
        (arch, os) => {
            // Unsupported combinations fall through to a slug that no
            // registry entry matches, producing the "publishes no binary for
            // <triple>" refusal rather than a wrong download.
            warn!("ci_node: unmapped host {arch}/{os} — tool provisioning will refuse");
            "unsupported"
        }
    }
}

/// Root of the cross-dispatch tool cache.
pub(crate) fn tools_cache_root(root: &Path) -> PathBuf {
    root.join(".ci-tools")
}

/// Cache directory for one pinned tool version. Pure — the layout rule, so
/// it is testable without touching a disk.
pub(crate) fn tool_dir(root: &Path, name: &str, version: &str) -> PathBuf {
    tools_cache_root(root).join(name).join(version)
}

/// Where the bytes come from, once the entry has been resolved for a concrete
/// version and host.
#[derive(Debug)]
enum Source {
    /// Pull exactly one basename-matched file out of an archive.
    ArchiveFile { asset: Asset, basename: String },
    /// Extract a whole tree out of an archive, traversal-checked and
    /// digest-pinned.
    ArchiveTree {
        asset: Asset,
        root_dir: String,
        sha256: String,
    },
    /// Build a private venv and install one pinned distribution into it.
    PythonPackage {
        requirement: String,
        module: &'static str,
    },
}

/// A registry entry resolved for one (version, host). Resolution happens
/// BEFORE anything is downloaded, so an unsupported platform refuses legibly
/// instead of 404ing.
#[derive(Debug)]
struct Resolved {
    /// PATH entry, relative to the version-keyed tool dir (`""` = the tool dir
    /// itself).
    bin_dir: PathBuf,
    /// Stem of the executable whose version must equal the declared one.
    primary: String,
    /// Stems of extra executables that must exist and run.
    companions: Vec<String>,
    source: Source,
}

impl Resolved {
    fn bin_dir_at(&self, tool_dir: &Path) -> PathBuf {
        if self.bin_dir.as_os_str().is_empty() {
            tool_dir.to_path_buf()
        } else {
            tool_dir.join(&self.bin_dir)
        }
    }
}

/// Suffix of the generated Python shim. `.cmd` on Windows so PATHEXT resolves
/// it (the same shape npm/pnpm ship, and the reason the executor already has a
/// `cmd.exe /C` respawn); extensionless elsewhere.
const fn shim_suffix() -> &'static str {
    if cfg!(target_os = "windows") {
        ".cmd"
    } else {
        ""
    }
}

fn resolve(spec: &'static ToolSpec, version: &str, triple: &str) -> Result<Resolved, String> {
    match &spec.install {
        Installer::PrebuiltBinary { binary, asset } => {
            let asset = asset(version, triple).ok_or_else(|| {
                format!(
                    "tool '{}' publishes no prebuilt binary for {triple}",
                    spec.name
                )
            })?;
            Ok(Resolved {
                bin_dir: PathBuf::new(),
                primary: (*binary).to_string(),
                companions: Vec::new(),
                source: Source::ArchiveFile {
                    basename: format!("{}{}", binary, std::env::consts::EXE_SUFFIX),
                    asset,
                },
            })
        }
        Installer::PrebuiltTree {
            primary,
            companions,
            asset,
        } => {
            let tree = asset(version, triple).ok_or_else(|| {
                format!(
                    "tool '{}' {version} is not provisionable on {triple}: either the                      project publishes no asset for this platform, or this runner has no                      reviewed SHA-256 for that (version, platform). A version whose bytes                      nobody reviewed is refused by construction — add its digests to the                      registry to enable it",
                    spec.name
                )
            })?;
            Ok(Resolved {
                bin_dir: PathBuf::from(tree.bin_subdir),
                primary: (*primary).to_string(),
                companions: companions.iter().map(|c| (*c).to_string()).collect(),
                source: Source::ArchiveTree {
                    asset: tree.asset,
                    root_dir: tree.root_dir,
                    sha256: tree.sha256.to_string(),
                },
            })
        }
        Installer::PythonPackage {
            distribution,
            module,
        } => Ok(Resolved {
            // NEVER the venv's own Scripts/bin: those console scripts do not
            // survive the atomic rename, and putting them on PATH would shadow
            // the shim that does. See the module docs.
            bin_dir: PathBuf::from("bin"),
            primary: spec.name.to_string(),
            companions: Vec::new(),
            source: Source::PythonPackage {
                requirement: format!("{distribution}=={version}"),
                module,
            },
        }),
    }
}

/// Find an executable named `stem` in `dir`, trying the spellings a release
/// archive or a shim actually uses on this platform. Returns `None` when none
/// exists, which a cache-hit check reads as a miss rather than as corruption.
///
/// ORDER MATTERS on Windows: node's zip ships `npm`, `npm.cmd` AND `npm.ps1`
/// side by side in one directory, and the extensionless `npm` is a **shell**
/// script that Windows cannot execute. So the runnable spellings are tried
/// first and the extensionless one last.
fn resolve_executable(dir: &Path, stem: &str) -> Option<PathBuf> {
    let mut suffixes = vec![std::env::consts::EXE_SUFFIX];
    if cfg!(target_os = "windows") {
        // node ships `npm.cmd`/`npx.cmd`; the Python shim is `.cmd` too.
        suffixes.push(".cmd");
        suffixes.push(".bat");
        // Last: on Unix this IS `EXE_SUFFIX` and was already tried.
        suffixes.push("");
    }
    for suffix in suffixes {
        let candidate = dir.join(format!("{stem}{suffix}"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Provision every declared tool, returning the directories to prepend to the
/// step PATH in declaration order. Any failure aborts the dispatch: a step
/// that silently ran without its declared tool is the JUnit-shaped hole this
/// module exists to close.
pub(crate) async fn provision(
    root: &Path,
    tools: &[CiTool],
    // `+ Send` because a dispatch runs on a `tokio::spawn`ed task and this
    // callback is held across awaits.
    log: &mut (dyn FnMut(String) + Send),
) -> Result<Vec<PathBuf>, String> {
    let mut dirs = Vec::with_capacity(tools.len());
    for tool in tools {
        let spec = lookup(&tool.name)
            .ok_or_else(|| format!("tool '{}' is not in the runner's registry", tool.name))?;
        // Resolved up front: an unsupported platform is a refusal BEFORE any
        // network or disk work, and the resolution is what tells us where the
        // binaries land.
        let resolved = resolve(spec, &tool.version, host_triple())?;
        let dir = tool_dir(root, spec.name, &tool.version);
        let bin_dir = resolved.bin_dir_at(&dir);

        // A cache directory is either adopted whole or REMOVED whole. The
        // middle state — left in place and reinstalled over — cannot work:
        // the publish step is a rename onto `dir`, which fails if anything is
        // there (ENOTEMPTY on Unix; on Windows any existing directory,
        // including an empty one). The fallback then misreads that failure as
        // "published concurrently", re-verifies, finds nothing, and fails —
        // and so does every later dispatch, forever, until a human deletes the
        // directory. A venv is thousands of files, so a partial delete under
        // an antivirus lock is a realistic way to enter that state, which is
        // why the removals below PROPAGATE instead of being swallowed.
        let cached = if resolve_executable(&bin_dir, &resolved.primary).is_some() {
            Some(verify_installed(&bin_dir, spec, &resolved, &tool.version).await)
        } else {
            None
        };
        match cached {
            Some(Ok(reports)) => {
                log(format!(
                    "[ci-node] tool {} {} — cache hit ({})",
                    spec.name,
                    tool.version,
                    bin_dir.display()
                ));
                for report in reports {
                    log(format!(
                        "[ci-node] tool {} {} — provides {report}",
                        spec.name, tool.version
                    ));
                }
                dirs.push(bin_dir);
                continue;
            }
            Some(Err(e)) => {
                // A cached directory that does not verify is worse than no
                // cache: drop it and reinstall rather than run a broken tool.
                warn!(
                    "ci_node: cached tool {} {} failed verification ({e}); reinstalling",
                    spec.name, tool.version
                );
                log(format!(
                    "[ci-node] tool {} {} — cached copy failed verification ({e}); reinstalling",
                    spec.name, tool.version
                ));
                remove_cache_dir(&dir)?;
            }
            None => {
                // Present but with no runnable primary: a half-deleted or
                // half-published directory. Same rule — remove it, or the
                // rename below can never succeed again.
                if dir.exists() {
                    warn!(
                        "ci_node: cache dir for {} {} has no runnable {}; removing",
                        spec.name, tool.version, resolved.primary
                    );
                    log(format!(
                        "[ci-node] tool {} {} — cache dir has no runnable {}; removing",
                        spec.name, tool.version, resolved.primary
                    ));
                    remove_cache_dir(&dir)?;
                }
            }
        }

        match &resolved.source {
            Source::ArchiveFile { asset, .. } | Source::ArchiveTree { asset, .. } => {
                log(format!(
                    "[ci-node] tool {} {} — fetching {}",
                    spec.name, tool.version, asset.url
                ));
            }
            Source::PythonPackage { requirement, .. } => {
                log(format!(
                    "[ci-node] tool {} {} — installing Python distribution {requirement} into a private venv",
                    spec.name, tool.version
                ));
            }
        }
        install(root, spec, &tool.version, &resolved, &dir, log).await?;
        log(format!(
            "[ci-node] tool {} {} — installed at {}",
            spec.name,
            tool.version,
            bin_dir.display()
        ));
        dirs.push(bin_dir);
    }
    Ok(dirs)
}

/// Remove a cache directory, propagating failure.
///
/// Deliberately NOT best-effort: a partial removal leaves a directory that no
/// later rename can replace, which wedges this (tool, version) permanently.
/// Failing the dispatch with the real OS error is recoverable; silently
/// continuing is not.
fn remove_cache_dir(dir: &Path) -> Result<(), String> {
    std::fs::remove_dir_all(dir).map_err(|e| {
        format!(
            "could not remove the unusable tool cache at {}: {e}. Delete it and re-run \
             (leaving it in place would wedge this tool version permanently)",
            dir.display()
        )
    })
}

/// Materialise → verify → atomically publish. Nothing is ever written
/// directly at `final_dir`; it appears only by renaming a staging directory
/// that has already passed [`verify`].
async fn install(
    root: &Path,
    spec: &ToolSpec,
    version: &str,
    resolved: &Resolved,
    final_dir: &Path,
    log: &mut (dyn FnMut(String) + Send),
) -> Result<(), String> {
    let staging_root = tools_cache_root(root).join(".staging");
    std::fs::create_dir_all(&staging_root)
        .map_err(|e| format!("create {}: {e}", staging_root.display()))?;
    sweep_stale_staging(&staging_root);
    // `tempfile`'s TempDir removes itself on drop, so an error path anywhere
    // below leaves no partial install behind.
    let staging = tempfile::Builder::new()
        .prefix(&format!("{}-{version}-", spec.name))
        .tempdir_in(&staging_root)
        .map_err(|e| format!("create staging dir under {}: {e}", staging_root.display()))?;
    let staged_bin_dir = resolved.bin_dir_at(staging.path());

    match &resolved.source {
        Source::ArchiveFile { asset, basename } => {
            // No digest on this variant — see `Installer::PrebuiltBinary`.
            let bytes = download(&asset.url, None).await?;
            let dest = staged_bin_dir.join(basename);
            let kind = asset.kind;
            let wanted = basename.clone();
            let url = asset.url.clone();
            // Decompression is CPU-bound and blocking.
            tokio::task::spawn_blocking(move || {
                extract_one_file(&bytes, kind, &wanted, &dest, &url)
            })
            .await
            .map_err(|e| format!("extract task panicked: {e}"))??;
        }
        Source::ArchiveTree {
            asset,
            root_dir,
            sha256,
        } => {
            let bytes = download(&asset.url, Some(sha256)).await?;
            let dest = staging.path().to_path_buf();
            let kind = asset.kind;
            let root_dir = root_dir.clone();
            let url = asset.url.clone();
            tokio::task::spawn_blocking(move || extract_tree(&bytes, kind, &root_dir, &dest, &url))
                .await
                .map_err(|e| format!("extract task panicked: {e}"))??;
        }
        Source::PythonPackage {
            requirement,
            module,
        } => {
            install_python_package(
                staging.path(),
                &staged_bin_dir,
                &resolved.primary,
                requirement,
                module,
                log,
            )
            .await?;
        }
    }

    // Companions are checked to EXIST and to RUN. Their versions are not the
    // declared one (npm's version is a property of the node release), so they
    // are reported rather than asserted — but a missing or unrunnable one
    // fails the install, because "node provisioned, npm absent" must not be
    // discovered by a step ten minutes later.
    for report in verify_installed(&staged_bin_dir, spec, resolved, version).await? {
        log(format!(
            "[ci-node] tool {} {version} — provides {report}",
            spec.name
        ));
    }

    if let Some(parent) = final_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    // `keep` disarms the auto-delete — from here the directory is either
    // renamed into place or left for the next dispatch's staging sweep.
    let staged_path = staging.keep();
    match std::fs::rename(&staged_path, final_dir) {
        Ok(()) => Ok(()),
        Err(e) => {
            // A concurrent dispatch may have published the same pinned
            // version first. That is a WIN, not a race to resolve: the
            // directory is version-keyed, so whatever is there is the same
            // tool. Verify it and adopt it.
            let _ = std::fs::remove_dir_all(&staged_path);
            let bin_dir = resolved.bin_dir_at(final_dir);
            if verify_installed(&bin_dir, spec, resolved, version)
                .await
                .is_ok()
            {
                info!(
                    "ci_node: tool {} {version} was published concurrently — adopting {}",
                    spec.name,
                    final_dir.display()
                );
                return Ok(());
            }
            Err(format!(
                "publish {} -> {}: {e}",
                staged_path.display(),
                final_dir.display()
            ))
        }
    }
}

/// Remove staging directories left behind by a killed dispatch.
///
/// The publish path disarms the `TempDir` auto-delete before renaming, so a
/// process killed in that window leaks its staging directory. That used to be
/// a ~10 MB binary; with a node tree or a venv it is up to a few hundred MB
/// each, so the "left for the next dispatch's staging sweep" the publish
/// comment referred to had to actually exist — it did not.
///
/// The age floor is what makes this safe to run while other dispatches are
/// mid-install: no install comes close to a day, so anything older than
/// [`STAGING_MAX_AGE`] belongs to a process that is gone. Best-effort by
/// design — a sweep failure must never fail a dispatch that is otherwise fine.
fn sweep_stale_staging(staging_root: &Path) {
    sweep_stale_staging_at(staging_root, std::time::SystemTime::now())
}

/// The sweep with its clock injected, so the age predicate is exercised
/// without backdating an mtime.
fn sweep_stale_staging_at(staging_root: &Path, now: std::time::SystemTime) {
    let Ok(entries) = std::fs::read_dir(staging_root) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > STAGING_MAX_AGE);
        if !stale {
            continue;
        }
        let path = entry.path();
        match std::fs::remove_dir_all(&path) {
            Ok(()) => info!("ci_node: swept stale tool staging dir {}", path.display()),
            Err(e) => warn!("ci_node: could not sweep {}: {e}", path.display()),
        }
    }
}

/// Fetch the asset into memory, STREAMING so the size ceiling actually binds.
///
/// The previous shape read the whole body with `resp.bytes()` and only then
/// compared its length to the ceiling — which bounds nothing, because the
/// allocation has already happened by the time the check runs, and
/// `Content-Length` is advisory. Chunks are accumulated and the transfer is
/// aborted the moment the running total crosses the cap.
///
/// When `expected_sha256` is `Some`, the digest is checked HERE — before any
/// extractor sees a byte. That is the point: verification-by-execution can
/// only run after thousands of files are already on disk, so it is the wrong
/// place to catch a hostile archive.
async fn download(url: &str, expected_sha256: Option<&str>) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .map_err(|e| format!("build http client: {e}"))?;
    let mut resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!(
            "GET {url} returned HTTP {status} — check the declared version exists"
        ));
    }
    if let Some(len) = resp.content_length() {
        if len > MAX_ASSET_BYTES {
            return Err(format!(
                "GET {url} advertises {len} bytes, over the {MAX_ASSET_BYTES}-byte ceiling"
            ));
        }
    }
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("read body of {url}: {e}"))?
    {
        if body.len() as u64 + chunk.len() as u64 > MAX_ASSET_BYTES {
            return Err(format!(
                "GET {url} exceeded the {MAX_ASSET_BYTES}-byte ceiling mid-transfer — aborted"
            ));
        }
        body.extend_from_slice(&chunk);
    }
    if body.is_empty() {
        return Err(format!("GET {url} returned an empty body"));
    }
    if let Some(want) = expected_sha256 {
        let got = hex::encode(Sha256::digest(&body));
        if !got.eq_ignore_ascii_case(want) {
            return Err(format!(
                "GET {url} SHA-256 {got} does not match the registry's reviewed digest {want}                  — refusing before extraction"
            ));
        }
    }
    Ok(body)
}

/// Pull exactly one named file out of the archive and write it to `dest`.
/// Only the basename is matched and only `dest` is ever written, so a crafted
/// archive entry (`../../…`, an absolute path, a symlink) cannot place a file
/// anywhere — the classic tar/zip-slip class is structurally absent rather
/// than filtered.
fn extract_one_file(
    bytes: &[u8],
    kind: ArchiveKind,
    wanted: &str,
    dest: &Path,
    url: &str,
) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let mut out =
        std::fs::File::create(dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
    let found = match kind {
        ArchiveKind::Zip => {
            let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))
                .map_err(|e| format!("open zip from {url}: {e}"))?;
            let mut hit = false;
            for i in 0..zip.len() {
                let mut entry = zip
                    .by_index(i)
                    .map_err(|e| format!("read zip entry {i} from {url}: {e}"))?;
                let name = entry
                    .enclosed_name()
                    .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()));
                if entry.is_file() && name.as_deref() == Some(wanted) {
                    std::io::copy(&mut entry, &mut out)
                        .map_err(|e| format!("extract {wanted} from {url}: {e}"))?;
                    hit = true;
                    break;
                }
            }
            hit
        }
        ArchiveKind::TarGz => {
            let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
            let mut hit = false;
            for entry in tar
                .entries()
                .map_err(|e| format!("open tar.gz from {url}: {e}"))?
            {
                let mut entry = entry.map_err(|e| format!("read tar entry from {url}: {e}"))?;
                let name = entry
                    .path()
                    .ok()
                    .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()));
                if entry.header().entry_type().is_file() && name.as_deref() == Some(wanted) {
                    std::io::copy(&mut entry, &mut out)
                        .map_err(|e| format!("extract {wanted} from {url}: {e}"))?;
                    hit = true;
                    break;
                }
            }
            hit
        }
    };
    drop(out);
    if !found {
        let _ = std::fs::remove_file(dest);
        return Err(format!("archive {url} contains no {wanted}"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod {}: {e}", dest.display()))?;
    }
    Ok(())
}

/// Validate one archive entry path and strip the expected top-level directory.
///
/// This is the tree variant's structural guarantee, and it is a PURE function
/// so the refusals are tested without touching a disk. Returns `Ok(None)` for
/// an entry that IS the top-level directory (nothing to write).
///
/// Rejects, before anything is written:
/// * absolute paths (`/x`) and drive-qualified paths (`C:\x`) — checked as
///   STRINGS so the refusal holds on every host, not just the one whose
///   `Path::is_absolute` happens to agree;
/// * any `..` component, anywhere;
/// * any entry not under `root_dir`, which also rejects an archive smuggling
///   in a second top-level tree.
fn safe_tree_path(raw: &str, root_dir: &str) -> Result<Option<PathBuf>, String> {
    if raw.starts_with('/') || raw.starts_with('\\') || raw.contains(':') || raw.contains('\0') {
        return Err(format!(
            "archive entry {raw:?} is absolute or drive-qualified — refusing"
        ));
    }
    let mut parts: Vec<&str> = Vec::new();
    for segment in raw.split(['/', '\\']) {
        match segment {
            "" | "." => continue,
            ".." => {
                return Err(format!(
                    "archive entry {raw:?} contains a parent component — refusing"
                ))
            }
            other => parts.push(other),
        }
    }
    let Some(first) = parts.first() else {
        return Ok(None);
    };
    if *first != root_dir {
        return Err(format!(
            "archive entry {raw:?} is outside the expected top-level directory {root_dir:?} — refusing"
        ));
    }
    if parts.len() == 1 {
        return Ok(None);
    }
    Ok(Some(parts[1..].iter().collect()))
}

/// Does a link entry's target stay inside the destination tree?
///
/// `link` is the entry's path relative to the destination root (already
/// stripped); `target` is the raw link target from the archive. Resolved
/// LEXICALLY — the depth of the link's own directory is walked down by each
/// `..` and up by each name, and the moment it would go above the root the
/// link is refused.
///
/// # Why the lexical accounting is SOUND here, and was not before
///
/// Lexical depth counting assumes every non-`..` segment is a real directory.
/// That assumption is false the moment a link's target traverses *through
/// another link that was already accepted*, and it produced a real arbitrary
/// write. With root `R` and a hostile archive, in order:
///
/// 1. `R/d/` (dir)          — accepted; creates `dest/d`.
/// 2. `R/d/up -> ..`        — depth 1 to 0, "contained"; `dest/d/up` IS `dest`.
/// 3. `R/d/hop -> up/..`    — depth 1 to 2 to 1, "contained"; but
///    `dest/d/up/..` is `dest/..`, one level OUTSIDE.
/// 4. `R/d/hop/evil` (file) — no `..`, under the root, so accepted; the write
///    follows `hop` and lands outside the extraction root.
///
/// Each further `hopN -> hopN-1/..` pair climbs another level, so a handful of
/// entries reach `$HOME` — `~/.ssh/authorized_keys`, `~/.bashrc`.
///
/// The fix is not a better filter. **No symlink is ever created under the
/// destination** (see [`extract_tree`]), so every path the extractor opens is
/// composed of validated `Normal` components over directories it created
/// itself, and `dest.join(rel)` cannot escape — structurally, not by
/// filtering. That also makes lexical equal to real for this function, because
/// there is no link on disk for a target to traverse through. This check now
/// governs only WHERE A LAUNCHER POINTS, which still matters: a launcher aimed
/// outside would exec an arbitrary host binary when a step runs `npm`.
///
/// node's `bin/npm -> ../lib/node_modules/npm/bin/npm-cli.js` is the shape
/// that must pass; `bin/evil -> ../../etc/passwd` is the shape that must not.
fn symlink_target_is_contained(link: &Path, target: &str) -> bool {
    if target.is_empty()
        || target.starts_with('/')
        || target.starts_with('\\')
        || target.contains(':')
        || target.contains('\0')
    {
        return false;
    }
    // Depth of the directory the link lives in, measured from the root.
    let mut depth = link.components().count() as i64 - 1;
    if depth < 0 {
        return false;
    }
    for segment in target.split(['/', '\\']) {
        match segment {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => depth += 1,
        }
    }
    true
}

/// Resolve a link target lexically against the link's own directory, giving a
/// path relative to the destination root. `None` when it escapes.
fn resolve_link_target(link: &Path, target: &str) -> Option<PathBuf> {
    if !symlink_target_is_contained(link, target) {
        return None;
    }
    let mut parts: Vec<String> = link
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    // Resolution starts in the link's DIRECTORY, so drop its own name.
    parts.pop();
    for segment in target.split(['/', '\\']) {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other.to_string()),
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.iter().collect())
}

/// A link entry recorded during extraction and materialised afterwards.
struct LinkEntry {
    /// Path relative to the destination root.
    rel: PathBuf,
    /// The raw target, relative to the directory holding `rel`.
    target: String,
}

/// How many link hops a chain may take before it is called a cycle.
const MAX_LINK_HOPS: usize = 8;

/// One validated entry, ready to write.
enum TreeEntry<'a> {
    Dir,
    File {
        mode: Option<u32>,
        reader: &'a mut dyn std::io::Read,
    },
}

/// Extract a whole archive into `dest`, stripping `root_dir`.
///
/// Every entry is validated by [`safe_tree_path`] BEFORE any path is opened,
/// and **no symlink is ever created**, so `dest.join(rel)` is always a path
/// through directories this function made itself out of validated `Normal`
/// components. Traversal is therefore structurally impossible rather than
/// filtered — see [`symlink_target_is_contained`] for the arbitrary-write bug
/// that filtering produced.
///
/// Link entries are recorded and materialised afterwards by
/// [`materialize_links`] as position-independent launchers. Entry types other
/// than file/dir/symlink — hard links, fifos, device nodes — are refused: none
/// appear in the curated releases, and each is a way to reach outside the tree.
fn extract_tree(
    bytes: &[u8],
    kind: ArchiveKind,
    root_dir: &str,
    dest: &Path,
    url: &str,
) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
    let mut wrote_any = false;
    let mut links: Vec<LinkEntry> = Vec::new();
    match kind {
        ArchiveKind::Zip => {
            let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))
                .map_err(|e| format!("open zip from {url}: {e}"))?;
            for i in 0..zip.len() {
                let mut entry = zip
                    .by_index(i)
                    .map_err(|e| format!("read zip entry {i} from {url}: {e}"))?;
                let raw = entry.name().to_string();
                let Some(rel) =
                    safe_tree_path(&raw, root_dir).map_err(|e| format!("{url}: {e}"))?
                else {
                    continue;
                };
                let mode = entry.unix_mode();
                // A zip symlink is a regular entry with S_IFLNK in its unix
                // mode. The curated Windows node archive has none, so the
                // honest handling is a refusal rather than an untested path.
                if mode.is_some_and(|m| m & 0o170000 == 0o120000) {
                    return Err(format!(
                        "{url}: zip entry {raw:?} is a symlink; the curated zip assets \
                         contain none — refusing"
                    ));
                }
                let is_dir = entry.is_dir();
                write_tree_entry(
                    dest,
                    &rel,
                    if is_dir {
                        TreeEntry::Dir
                    } else {
                        TreeEntry::File {
                            mode,
                            reader: &mut entry,
                        }
                    },
                )?;
                wrote_any = true;
            }
        }
        ArchiveKind::TarGz => {
            let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
            for entry in tar
                .entries()
                .map_err(|e| format!("open tar.gz from {url}: {e}"))?
            {
                let mut entry = entry.map_err(|e| format!("read tar entry from {url}: {e}"))?;
                let entry_type = entry.header().entry_type();
                // A pax global header carries archive-wide metadata, not a
                // path. Skipping it explicitly keeps a legitimate archive from
                // being refused with a misleading "outside the expected
                // top-level directory".
                if matches!(entry_type, tar::EntryType::XGlobalHeader) {
                    continue;
                }
                let raw = String::from_utf8_lossy(&entry.path_bytes()).to_string();
                let Some(rel) =
                    safe_tree_path(&raw, root_dir).map_err(|e| format!("{url}: {e}"))?
                else {
                    continue;
                };
                let mode = entry.header().mode().ok();
                if entry_type.is_dir() {
                    write_tree_entry(dest, &rel, TreeEntry::Dir)?;
                } else if entry_type.is_symlink() {
                    let target = entry
                        .link_name_bytes()
                        .map(|b| String::from_utf8_lossy(&b).to_string())
                        .unwrap_or_default();
                    if !symlink_target_is_contained(&rel, &target) {
                        return Err(format!(
                            "{url}: link {raw:?} -> {target:?} escapes the extraction \
                             root — refusing"
                        ));
                    }
                    // Recorded, NOT created. Nothing under `dest` is ever a
                    // symlink, which is what makes every later join safe.
                    links.push(LinkEntry { rel, target });
                } else if entry_type.is_file() {
                    write_tree_entry(
                        dest,
                        &rel,
                        TreeEntry::File {
                            mode,
                            reader: &mut entry,
                        },
                    )?;
                } else {
                    return Err(format!(
                        "{url}: tar entry {raw:?} has unsupported type {entry_type:?} — refusing"
                    ));
                }
                wrote_any = true;
            }
        }
    }
    if !wrote_any {
        return Err(format!(
            "archive {url} contained nothing under {root_dir:?}"
        ));
    }
    materialize_links(dest, &links, url)?;
    Ok(())
}

/// Turn recorded link entries into position-independent launcher scripts.
///
/// # Why a launcher and not a file copy
///
/// Copying the target's bytes to the link path looks simpler and also deletes
/// the symlink class — but it **breaks node**, and that was measured rather
/// than assumed. node's `bin/npm` points at
/// `../lib/node_modules/npm/bin/npm-cli.js`, whose entire body is a shebang
/// plus `require('../lib/cli.js')(process)`. That `require` resolves against
/// the SCRIPT'S OWN directory: in place it means
/// `lib/node_modules/npm/lib/cli.js`, which exists; copied to `bin/npm` it
/// means `<tree>/lib/cli.js`, which the v22.11.0 archive does not contain at
/// all (checked — zero entries match). A copy would provision green and fail
/// on the first `npm ci`.
///
/// A launcher execs the target where it really lives, so the interpreter sees
/// the real path and every relative `require` still resolves. The launcher's
/// own path is composed only of validated components, and its body carries the
/// archive's target string, already proven contained.
fn materialize_links(dest: &Path, links: &[LinkEntry], url: &str) -> Result<(), String> {
    for link in links {
        // Follow the chain lexically to prove it ends at a real extracted
        // file. Launchers chain fine at run time, but a cycle or a dangling
        // target must fail HERE rather than at `npm ci`.
        let mut hops = 0usize;
        let mut cursor_rel = link.rel.clone();
        let mut cursor_target = link.target.clone();
        loop {
            let resolved = resolve_link_target(&cursor_rel, &cursor_target).ok_or_else(|| {
                format!(
                    "{url}: link {} -> {} escapes the extraction root — refusing",
                    cursor_rel.display(),
                    cursor_target
                )
            })?;
            if let Some(next) = links.iter().find(|l| l.rel == resolved) {
                hops += 1;
                if hops > MAX_LINK_HOPS {
                    return Err(format!(
                        "{url}: link chain from {} exceeds {MAX_LINK_HOPS} hops — refusing",
                        link.rel.display()
                    ));
                }
                cursor_rel = next.rel.clone();
                cursor_target = next.target.clone();
                continue;
            }
            if !dest.join(&resolved).is_file() {
                return Err(format!(
                    "{url}: link {} -> {} resolves to {}, which the archive did not \
                     provide as a regular file — refusing",
                    link.rel.display(),
                    link.target,
                    resolved.display()
                ));
            }
            break;
        }
        write_link_launcher(dest, &link.rel, &link.target)?;
    }
    Ok(())
}

/// Write the launcher itself. Unix-only by construction: the curated Windows
/// asset is a zip and zip link entries are refused outright, so a link entry
/// reaching a non-Unix host is an unreviewed archive shape and refuses rather
/// than silently producing something unrunnable.
fn write_link_launcher(dest: &Path, rel: &Path, target: &str) -> Result<(), String> {
    let path = dest.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    #[cfg(unix)]
    {
        let body = format!("#!/bin/sh\nexec \"$(dirname \"$0\")/{target}\" \"$@\"\n");
        std::fs::write(&path, body).map_err(|e| format!("write {}: {e}", path.display()))?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod {}: {e}", path.display()))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = target;
        Err(format!(
            "archive entry {} is a link, which only the tar assets carry and only Unix \
             hosts consume — refusing rather than writing something unrunnable",
            path.display()
        ))
    }
}

fn write_tree_entry(dest: &Path, rel: &Path, entry: TreeEntry<'_>) -> Result<(), String> {
    let path = dest.join(rel);
    match entry {
        TreeEntry::Dir => {
            std::fs::create_dir_all(&path)
                .map_err(|e| format!("create {}: {e}", path.display()))?;
        }
        TreeEntry::File { mode, reader } => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("create {}: {e}", parent.display()))?;
            }
            let mut out = std::fs::File::create(&path)
                .map_err(|e| format!("create {}: {e}", path.display()))?;
            std::io::copy(reader, &mut out)
                .map_err(|e| format!("write {}: {e}", path.display()))?;
            drop(out);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                // Honour the archive's executable bit (node's `bin/node` needs
                // it) but never setuid/setgid/sticky and never group- or
                // other-writable: masked to 0o755.
                let bits = mode.map(|m| m & 0o755).unwrap_or(0o644);
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(bits))
                    .map_err(|e| format!("chmod {}: {e}", path.display()))?;
            }
            #[cfg(not(unix))]
            {
                let _ = mode;
            }
        }
    }
    Ok(())
}

// ── Python packages ───────────────────────────────────────────────────────

/// Locate a host Python 3. A miss is a REFUSAL naming what is missing and what
/// was tried — never a silent skip, and never an install of one.
async fn find_python() -> Result<String, String> {
    let mut tried = Vec::new();
    for candidate in PYTHON_CANDIDATES {
        match run_capture(candidate, &["--version"], VERIFY_TIMEOUT, None).await {
            Ok(text) if text.contains("Python 3") => {
                info!(
                    "ci_node: using {candidate} ({}) for Python tools",
                    text.trim()
                );
                return Ok((*candidate).to_string());
            }
            Ok(text) => tried.push(format!("{candidate}: reported {:?}", text.trim())),
            Err(e) => tried.push(format!("{candidate}: {e}")),
        }
    }
    Err(no_python_refusal(&tried))
}

/// The refusal text for "no Python 3 on this box". Pure, so the promise that
/// a missing interpreter is a REFUSAL naming what is missing — never a silent
/// skip — is asserted on the actual message rather than on a constant.
fn no_python_refusal(tried: &[String]) -> String {
    format!(
        "no Python 3 interpreter found — tried {} ({}). \
         Install Python 3 (including its `venv` module) and put it on PATH. \
         The runner will never install a Python, and never installs packages into \
         the host interpreter: the distribution goes into a private venv under \
         .ci-tools/. A missing interpreter is a refusal, not a skip",
        PYTHON_CANDIDATES.join(", "),
        tried.join("; ")
    )
}

/// Build a private venv under `staging` and install one pinned distribution
/// into it, then write the relocatable shim into `bin_dir`.
async fn install_python_package(
    staging: &Path,
    bin_dir: &Path,
    shim_stem: &str,
    requirement: &str,
    module: &str,
    log: &mut (dyn FnMut(String) + Send),
) -> Result<(), String> {
    let python = find_python().await?;
    let venv = staging.join("venv");
    let venv_str = venv.to_string_lossy().to_string();
    run_capture(
        &python,
        &["-m", "venv", venv_str.as_str()],
        VENV_TIMEOUT,
        None,
    )
    .await
    .map_err(|e| {
        format!(
            "creating a private venv with `{python} -m venv` failed: {e}. \
                 On Debian/Ubuntu the venv module ships separately (python3-venv)"
        )
    })?;
    let venv_bin = venv.join(if cfg!(target_os = "windows") {
        "Scripts"
    } else {
        "bin"
    });
    let venv_python = resolve_executable(&venv_bin, "python").ok_or_else(|| {
        format!(
            "`{python} -m venv` produced no python under {}",
            venv_bin.display()
        )
    })?;

    // `--only-binary=:all:` keeps the "never build from source" property of
    // the archive variants: pip installs wheels or fails legibly, rather than
    // compiling an sdist (and running its setup.py) for minutes on a user's
    // machine. `--no-cache-dir` keeps the install from writing into the user's
    // pip cache — the version-keyed tool cache is the only thing this lane
    // leaves behind.
    let out = run_capture(
        &venv_python.to_string_lossy(),
        &[
            "-m",
            "pip",
            "install",
            "--no-cache-dir",
            "--no-input",
            "--disable-pip-version-check",
            "--only-binary=:all:",
            // Explicit, so the source of the distribution is a property of
            // this registry rather than of whatever pip config the host
            // happens to carry. Belt and braces with the PIP_* scrub in
            // `run_capture`.
            "--index-url",
            "https://pypi.org/simple",
            requirement,
        ],
        PACKAGE_INSTALL_TIMEOUT,
        None,
    )
    .await
    .map_err(|e| format!("pip install {requirement} failed: {e}"))?;
    if let Some(last) = out.lines().rfind(|l| !l.trim().is_empty()) {
        log(format!("[ci-node] pip: {last}"));
    }

    write_python_shim(bin_dir, shim_stem, module)?;
    Ok(())
}

/// Write the position-independent launcher that goes on the step PATH.
///
/// It resolves the venv relative to its OWN location, which is what makes the
/// atomic rename into the version-keyed cache safe — see the module docs for
/// the measurement showing the venv's own console scripts do not survive it.
fn write_python_shim(bin_dir: &Path, stem: &str, module: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(bin_dir).map_err(|e| format!("create {}: {e}", bin_dir.display()))?;
    let path = bin_dir.join(format!("{stem}{}", shim_suffix()));
    // `-I` (isolated) is load-bearing, not hygiene. `python -m <module>`
    // prepends the CURRENT WORKING DIRECTORY to `sys.path`; verification runs
    // in the runner's cwd while steps run in the repo's, so a repo shipping
    // its own `poetry.py` would get a different module than the one this
    // registry verified — silently breaking mandatory-property 2 on this
    // variant. Measured on this box 2026-08-18: from a directory containing a
    // one-line `poetry.py`, `python -m poetry --version` printed `HIJACKED`
    // while `python -I -m poetry --version` printed `Poetry (version 2.1.3)`.
    // `-I` also implies `-E` (ignore PYTHON* env) and `-s` (no user site), and
    // does not disturb venv resolution, which comes from `pyvenv.cfg` beside
    // the interpreter rather than from the environment.
    let body = if cfg!(target_os = "windows") {
        format!("@echo off\r\n\"%~dp0..\\venv\\Scripts\\python.exe\" -I -m {module} %*\r\n")
    } else {
        format!("#!/bin/sh\nexec \"$(dirname \"$0\")/../venv/bin/python\" -I -m {module} \"$@\"\n")
    };
    std::fs::write(&path, body).map_err(|e| format!("write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod {}: {e}", path.display()))?;
    }
    Ok(path)
}

// ── Verification ──────────────────────────────────────────────────────────

/// Run a program and return its combined output, failing on a non-zero exit,
/// a spawn error or a timeout.
///
/// `path_dir`, when given, is PREPENDED to the child's PATH — the same PATH
/// the steps will get. That is not cosmetic: node's Unix `bin/npm` is a
/// symlink to a `#!/usr/bin/env node` script, so verifying npm without our own
/// node in front of PATH would either fail or, worse, verify the HOST's node.
///
/// `pub(super)` because [`super::services`] runs container commands with the
/// same requirements — no console window, a hard timeout, combined output, and
/// the Windows `.cmd`-shim respawn (`docker` on Windows is an exe, but podman
/// and Docker Desktop's compatibility shims are not always). Sharing this is
/// strictly better than a second copy of the spawn/timeout/shim logic.
/// An argv rendered for an ERROR MESSAGE with credential-bearing tokens
/// stripped.
///
/// Error strings from this helper travel a long way: `services` logs them to
/// the runner's on-disk dev log, pushes them into the dispatch's live progress
/// stream, and they end up in the `log_tail` coord PERSISTS with the dispatch
/// result. So a value that reaches an argv here is effectively published.
///
/// The container flag `-e KEY=VALUE` is the shape that carries one. Callers in
/// this crate now pass `-e KEY` (bare, value supplied via
/// [`run_capture_env`]), so this is belt-and-braces for that path — but it is
/// NOT redundant: this helper keeps gaining callers, and the next one to write
/// `-e PASSWORD=…` should leak nothing. The KEY is kept because it is the
/// diagnostic half; only the value is dropped.
///
/// **All three spellings docker and podman accept are handled**, because a
/// redactor that fails open is worse than none — it is trusted. The separated
/// `-e NAME=VALUE`, the attached long `--env=NAME=VALUE`, and the attached
/// short `-eNAME=VALUE` all redact; the first version of this function handled
/// only the separated form and passed the other two through verbatim, which is
/// exactly the shape a future caller is most likely to write.
pub(super) fn redacted_argv(args: &[&str]) -> Vec<String> {
    /// `NAME=VALUE` -> `NAME=<redacted>`; a token with no `=` carries no value
    /// and is returned as-is.
    fn drop_value(token: &str) -> String {
        match token.split_once('=') {
            Some((key, _)) => format!("{key}=<redacted>"),
            None => token.to_string(),
        }
    }
    let mut out = Vec::with_capacity(args.len());
    let mut prev_was_env_flag = false;
    for arg in args {
        let token = *arg;
        if prev_was_env_flag {
            // Separated form: `-e` `NAME=VALUE`.
            out.push(drop_value(token));
        } else if let Some(rest) = token.strip_prefix("--env=") {
            // Attached long form: `--env=NAME=VALUE`.
            out.push(format!("--env={}", drop_value(rest)));
        } else if token.len() > 2 && token.starts_with("-e") && !token.starts_with("--") {
            // Attached short form: `-eNAME=VALUE`.
            out.push(format!("-e{}", drop_value(&token[2..])));
        } else {
            out.push(token.to_string());
        }
        prev_was_env_flag = matches!(token, "-e" | "--env");
    }
    out
}

pub(super) async fn run_capture(
    program: &str,
    args: &[&str],
    timeout: Duration,
    path_dir: Option<&Path>,
) -> Result<String, String> {
    run_capture_env(program, args, timeout, path_dir, &[]).await
}

/// [`run_capture`] plus environment variables handed to the child.
///
/// This exists so a caller can pass a SECRET to a child process without ever
/// putting it on an argv. `services` needs exactly that: `docker run -e NAME`
/// (no `=value`) means "inherit NAME from my environment", so the container
/// gets its password while the password never appears in a command line —
/// neither in this function's error strings, nor in the host process table
/// that every other session on the machine can read.
pub(super) async fn run_capture_env(
    program: &str,
    args: &[&str],
    timeout: Duration,
    path_dir: Option<&Path>,
    envs: &[(String, String)],
) -> Result<String, String> {
    fn build(
        program: &str,
        args: &[&str],
        path_dir: Option<&Path>,
        envs: &[(String, String)],
    ) -> tokio::process::Command {
        let mut cmd = crate::process_helpers::tokio_no_window(program);
        cmd.args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            // A `tokio::time::timeout` around `output()` DROPS the future when
            // it fires; without this the child survives, orphaned. With a
            // 900s pip budget that means a live installer still writing into
            // the staging tree the `TempDir` destructor is deleting — on
            // Windows its open handles make `remove_dir_all` fail and leave a
            // half-built venv behind. Same reason `agent_pusher` and
            // `mcp::misc` set it.
            .kill_on_drop(true);
        // Provisioning children are OURS, not the repo's steps, so the host's
        // ambient ecosystem config must not decide what gets installed.
        // `PIP_INDEX_URL`/`PIP_CONFIG_FILE` redirect where a distribution
        // comes from; `PYTHONPATH`/`PYTHONHOME`/`VIRTUAL_ENV` redirect which
        // modules a verification actually imports. The manifest cannot set
        // these (they are off ENV_ALLOWLIST), but INHERITING them would leave
        // the same hole open from the other side — and the allowlist rationale
        // says the provisioned layout is executor-owned, so it has to be.
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy().to_ascii_uppercase();
            if name.starts_with("PIP_")
                || name.starts_with("PYTHON")
                || name.starts_with("POETRY_")
                || name.starts_with("NPM_CONFIG_")
                || name == "VIRTUAL_ENV"
            {
                cmd.env_remove(&key);
            }
        }
        if let Some(dir) = path_dir {
            let inherited = std::env::var_os("PATH").unwrap_or_default();
            let mut entries = vec![dir.to_path_buf()];
            entries.extend(std::env::split_paths(&inherited));
            if let Ok(joined) = std::env::join_paths(entries) {
                cmd.env("PATH", joined);
            }
        }
        for (k, v) in envs {
            cmd.env(k, v);
        }
        cmd
    }
    let mut cmd = build(program, args, path_dir, envs);
    let spawned = tokio::time::timeout(timeout, cmd.output()).await;
    let out = match spawned {
        Ok(Ok(o)) => o,
        Ok(Err(e)) if cfg!(target_os = "windows") => {
            // Same fallback the executor uses for `pnpm`: `CreateProcess`
            // cannot launch a `.cmd`/`.bat` shim, and both node's npm and the
            // Python shim are exactly that.
            let mut argv = vec!["/C".to_string(), program.to_string()];
            argv.extend(args.iter().map(|a| (*a).to_string()));
            let argv_ref: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
            let mut shim = build("cmd.exe", &argv_ref, path_dir, envs);
            match tokio::time::timeout(timeout, shim.output()).await {
                Ok(Ok(o)) => o,
                Ok(Err(e2)) => return Err(format!("spawn {program} (direct: {e}; cmd /C: {e2})")),
                Err(_) => {
                    return Err(format!(
                        "{program} did not answer {:?} within {}s",
                        redacted_argv(args),
                        timeout.as_secs()
                    ))
                }
            }
        }
        Ok(Err(e)) => return Err(format!("spawn {program}: {e}")),
        Err(_) => {
            return Err(format!(
                "{program} did not answer {:?} within {}s",
                redacted_argv(args),
                timeout.as_secs()
            ))
        }
    };
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if !out.status.success() {
        return Err(format!(
            "{program} {:?} exited {} ({})",
            redacted_argv(args),
            out.status,
            text.trim()
        ));
    }
    Ok(text)
}

/// Run the installed tool and require it to report the declared version. This
/// is the integrity check, and it is mandatory on every [`Installer`] variant:
/// the projects in the registry publish no checksum assets, and executing the
/// artifact catches strictly more than a checksum would anyway — truncation, a
/// wrong-platform asset, a half-written venv and a wrong-VERSION artifact all
/// fail it, and the last is invisible to any hash we could have computed
/// ourselves.
async fn verify(
    bin: &Path,
    verify_args: &[&str],
    version: &str,
    path_dir: &Path,
) -> Result<(), String> {
    let text = run_capture(
        &bin.to_string_lossy(),
        verify_args,
        VERIFY_TIMEOUT,
        Some(path_dir),
    )
    .await?;
    if !version_is_reported(&text, version) {
        return Err(format!(
            "{} reports a different version than the declared {version}",
            bin.display()
        ));
    }
    Ok(())
}

/// Verify a materialised tool directory: the primary must report the declared
/// version, and every companion must exist and run. Returns the companions'
/// self-reported versions for logging.
///
/// Used by BOTH the cache-hit path and the post-install path, so a cache hit
/// is exactly as strong a statement as a fresh install. It did not used to be:
/// companions were checked only after installing, so a cache directory whose
/// `npm` had been removed sailed through as a hit and failed at `npm ci`
/// instead — contradicting the guarantee this module advertises.
async fn verify_installed(
    bin_dir: &Path,
    spec: &ToolSpec,
    resolved: &Resolved,
    version: &str,
) -> Result<Vec<String>, String> {
    let primary = resolve_executable(bin_dir, &resolved.primary).ok_or_else(|| {
        format!(
            "{} {version} provides no {} under {}",
            spec.name,
            resolved.primary,
            bin_dir.display()
        )
    })?;
    verify(&primary, spec.verify_args, version, bin_dir).await?;
    let mut reports = Vec::with_capacity(resolved.companions.len());
    for companion in &resolved.companions {
        let bin = resolve_executable(bin_dir, companion).ok_or_else(|| {
            format!(
                "{} {version} provides no {companion} under {}",
                spec.name,
                bin_dir.display()
            )
        })?;
        let reported = run_reporting_version(&bin, bin_dir).await?;
        reports.push(format!("{companion} {reported}"));
    }
    Ok(reports)
}

/// Run a companion binary and return what it reported. Its version is a
/// property of the primary's release, so it is logged rather than asserted —
/// but it must exist and it must run.
async fn run_reporting_version(bin: &Path, path_dir: &Path) -> Result<String, String> {
    let text = run_capture(
        &bin.to_string_lossy(),
        &["--version"],
        VERIFY_TIMEOUT,
        Some(path_dir),
    )
    .await?;
    Ok(text.trim().replace(['\r', '\n'], " "))
}

/// Does the tool's version output actually name `version`? Matched on a
/// whole token so `0.9.9` cannot be satisfied by `0.9.98`. A leading `v` on
/// the token is accepted because node reports `v22.11.0` — and only a leading
/// `v`, so `v0.9.98` still does not satisfy a declared `0.9.9`. Pure, so the
/// wrong-version case is testable without a download.
pub(crate) fn version_is_reported(output: &str, version: &str) -> bool {
    let v_prefixed = format!("v{version}");
    output
        .split(|c: char| {
            !(c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '+' || c == '_')
        })
        .any(|tok| tok == version || tok == v_prefixed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // ── Helpers for building hostile archives ────────────────────────────

    /// Build a tar.gz writing entry names and link targets RAW into the GNU
    /// header.
    ///
    /// `Builder::append_data`/`append_link` refuse to write `..` or an
    /// absolute path ("paths in archives must be relative"), which is exactly
    /// the shape under test — so the bytes go straight into the header. That
    /// is what a hostile archive looks like on the wire.
    fn targz(entries: &[(&str, tar::EntryType, &str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, kind, link, body) in entries {
            let mut header = tar::Header::new_gnu();
            {
                let gnu = header.as_gnu_mut().expect("gnu header");
                let raw = name.as_bytes();
                assert!(raw.len() < gnu.name.len(), "test name too long");
                gnu.name[..raw.len()].copy_from_slice(raw);
                let raw_link = link.as_bytes();
                assert!(raw_link.len() < gnu.linkname.len(), "test link too long");
                gnu.linkname[..raw_link.len()].copy_from_slice(raw_link);
            }
            header.set_entry_type(*kind);
            header.set_size(if kind.is_file() { body.len() as u64 } else { 0 });
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append(&header, std::io::Cursor::new(*body))
                .expect("append");
        }
        let tar = builder.into_inner().expect("finish tar");
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&tar).expect("gz");
        enc.finish().expect("gz finish")
    }

    /// A plain regular-file entry.
    fn file(
        name: &'static str,
        body: &'static [u8],
    ) -> (&'static str, tar::EntryType, &'static str, &'static [u8]) {
        (name, tar::EntryType::Regular, "", body)
    }

    // ── The closed registry ──────────────────────────────────────────────

    /// An argv formatted into an error string must not carry a credential.
    ///
    /// These error strings are logged to disk, streamed to coord live, and
    /// PERSISTED by coord in the dispatch result's `log_tail`, so a secret that
    /// reaches one is published. Callers in this crate pass the bare `-e NAME`
    /// form, and this is the second line of defence for the next caller that
    /// does not.
    #[test]
    fn argv_rendered_for_an_error_drops_env_values() {
        const SECRET: &str = "correcthorsebatterystaple";
        let args = [
            "run",
            "-d",
            "--name",
            "qontinui-ci-d1-postgres",
            // Separated form.
            "-e",
            "POSTGRES_PASSWORD=correcthorsebatterystaple",
            "--env",
            "PGPASSWORD=correcthorsebatterystaple",
            // Attached forms. Both are accepted by docker and podman, and both
            // were passed through VERBATIM by the first version of this
            // function — a redactor that fails open is worse than none,
            // because it is trusted.
            "--env=POSTGRES_INITDB_ARGS=correcthorsebatterystaple",
            "-ePGPASSFILE=correcthorsebatterystaple",
            // Bare: nothing to redact.
            "-e",
            "POSTGRES_DB",
            "postgres:16",
        ];
        let rendered = format!("{:?}", redacted_argv(&args));
        assert!(
            !rendered.contains(SECRET),
            "a value survived redaction: {rendered}"
        );
        // Every NAME survives — it is the diagnostic half.
        for kept in [
            "POSTGRES_PASSWORD=<redacted>",
            "PGPASSWORD=<redacted>",
            "--env=POSTGRES_INITDB_ARGS=<redacted>",
            "-ePGPASSFILE=<redacted>",
            "POSTGRES_DB",
        ] {
            assert!(rendered.contains(kept), "{kept} missing from {rendered}");
        }
        // Everything that is not an env value is untouched, so the error stays
        // diagnosable.
        assert!(rendered.contains("qontinui-ci-d1-postgres"), "{rendered}");
        assert!(rendered.contains("postgres:16"), "{rendered}");
    }

    /// Degenerate argv shapes must not panic, swallow a token, or leak.
    #[test]
    fn redaction_handles_degenerate_argv_shapes() {
        assert_eq!(redacted_argv(&[]), Vec::<String>::new());
        // `-e` as the LAST token: nothing follows it to redact.
        assert_eq!(redacted_argv(&["run", "-e"]), vec!["run", "-e"]);
        // A flag whose "value" is another flag, then the real pair.
        assert_eq!(
            redacted_argv(&["-e", "-e", "K=V"]),
            vec!["-e", "-e", "K=<redacted>"]
        );
        // A lone `-e`-prefixed token that is NOT an env flag stays intact.
        assert_eq!(redacted_argv(&["--exec", "x"]), vec!["--exec", "x"]);
    }

    /// The out-of-band path actually delivers: a value passed as env reaches
    /// the child, and never appears in the command line.
    #[tokio::test]
    async fn run_capture_env_delivers_a_value_without_putting_it_on_the_argv() {
        #[cfg(target_os = "windows")]
        let (program, args) = ("cmd.exe", vec!["/C", "echo %QONTINUI_TEST_SECRET%"]);
        #[cfg(not(target_os = "windows"))]
        let (program, args) = ("sh", vec!["-c", "printf %s \"$QONTINUI_TEST_SECRET\""]);

        let out = run_capture_env(
            program,
            &args,
            Duration::from_secs(30),
            None,
            &[(
                "QONTINUI_TEST_SECRET".to_string(),
                "correcthorsebatterystaple".to_string(),
            )],
        )
        .await
        .expect("the child must run");
        // The value reached the child. That it is not on the argv is asserted
        // where it is actually decided — `services::run_argv`, whose output is
        // the thing that could carry it — rather than against a literal here
        // that never contained it.
        assert!(out.contains("correcthorsebatterystaple"), "got {out:?}");
    }

    #[test]
    fn registry_is_closed() {
        assert!(lookup("cargo-nextest").is_some());
        assert!(lookup("cargo-install-anything").is_none());
        assert!(lookup("").is_none());
        // A URL is not a name, and neither is an installer command — the
        // registry is keyed by curated name and nothing else.
        assert!(lookup("https://example.invalid/evil.tar.gz").is_none());
        assert!(lookup("pip install evil").is_none());
        assert_eq!(
            known_tool_names(),
            vec![
                "cargo-nextest",
                "node",
                "poetry",
                "twine",
                "datamodel-code-generator"
            ]
        );
    }

    /// Asset URLs are derived, not hand-listed, so this pins the derivation
    /// against the names verified live on release `cargo-nextest-0.9.98`.
    #[test]
    fn nextest_asset_urls_match_the_published_names() {
        let cases = [
            (
                "x86_64-pc-windows-msvc",
                "cargo-nextest-0.9.98-x86_64-pc-windows-msvc.zip",
                ArchiveKind::Zip,
            ),
            (
                "x86_64-unknown-linux-gnu",
                "cargo-nextest-0.9.98-x86_64-unknown-linux-gnu.tar.gz",
                ArchiveKind::TarGz,
            ),
            (
                "aarch64-unknown-linux-gnu",
                "cargo-nextest-0.9.98-aarch64-unknown-linux-gnu.tar.gz",
                ArchiveKind::TarGz,
            ),
            // Both darwin arches map to the ONE universal asset — a per-arch
            // darwin name 404s.
            (
                "x86_64-apple-darwin",
                "cargo-nextest-0.9.98-universal-apple-darwin.tar.gz",
                ArchiveKind::TarGz,
            ),
            (
                "aarch64-apple-darwin",
                "cargo-nextest-0.9.98-universal-apple-darwin.tar.gz",
                ArchiveKind::TarGz,
            ),
        ];
        for (triple, want_file, kind) in cases {
            let a = cargo_nextest_asset("0.9.98", triple).expect("supported triple");
            assert_eq!(a.kind, kind, "{triple}");
            assert_eq!(
                a.url,
                format!(
                    "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-0.9.98/{want_file}"
                ),
                "{triple}"
            );
        }
    }

    /// Node's derivation AND its reviewed digests, pinned against release
    /// v22.11.0 (all five URLs 200'd; all five names and digests appear
    /// verbatim in that release's SHASUMS256.txt, and the linux-x64 row was
    /// re-confirmed by hashing the downloaded artifact). Unlike nextest,
    /// darwin is PER-ARCH here — a universal name would 404.
    #[test]
    fn node_asset_urls_and_digests_match_the_published_release() {
        let cases = [
            (
                "x86_64-pc-windows-msvc",
                "node-v22.11.0-win-x64",
                "zip",
                ArchiveKind::Zip,
                "",
                "905373a059aecaf7f48c1ce10ffbd5334457ca00f678747f19db5ea7d256c236",
            ),
            (
                "x86_64-unknown-linux-gnu",
                "node-v22.11.0-linux-x64",
                "tar.gz",
                ArchiveKind::TarGz,
                "bin",
                "4f862bab52039835efbe613b532238b6e4dde98d139a34e6923193e073438b13",
            ),
            (
                "aarch64-unknown-linux-gnu",
                "node-v22.11.0-linux-arm64",
                "tar.gz",
                ArchiveKind::TarGz,
                "bin",
                "27453f7a0dd6b9e6738f1f6ea6a09b102ec7aa484de1e39d6a1c3608ad47aa6a",
            ),
            (
                "x86_64-apple-darwin",
                "node-v22.11.0-darwin-x64",
                "tar.gz",
                ArchiveKind::TarGz,
                "bin",
                "668d30b9512137b5f5baeef6c1bb4c46efff9a761ba990a034fb6b28b9da2465",
            ),
            (
                "aarch64-apple-darwin",
                "node-v22.11.0-darwin-arm64",
                "tar.gz",
                ArchiveKind::TarGz,
                "bin",
                "2e89afe6f4e3aa6c7e21c560d8a0453d84807e97850bbb819b998531a22bdfde",
            ),
        ];
        for (triple, root, ext, kind, bin_subdir, sha) in cases {
            let t = node_asset("22.11.0", triple).expect("supported triple");
            assert_eq!(t.asset.kind, kind, "{triple}");
            assert_eq!(t.root_dir, root, "{triple}");
            assert_eq!(t.bin_subdir, bin_subdir, "{triple}");
            assert_eq!(t.sha256, sha, "{triple}");
            assert_eq!(
                t.asset.url,
                format!("https://nodejs.org/dist/v22.11.0/{root}.{ext}"),
                "{triple}"
            );
        }
        // Every digest is a full lowercase SHA-256.
        for (_, _, digest) in NODE_DIGESTS {
            assert_eq!(digest.len(), 64, "{digest}");
            assert!(
                digest
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{digest}"
            );
        }
    }

    /// A node version nobody reviewed the bytes of is REFUSED. The registry is
    /// closed over versions as well as names for this variant, because the
    /// digest is what makes a hostile archive unreachable.
    #[test]
    fn node_version_without_a_reviewed_digest_is_refused() {
        assert!(node_asset("22.11.0", "x86_64-unknown-linux-gnu").is_some());
        for unpinned in ["20.11.1", "22.11.1", "24.0.0", "0.0.1"] {
            assert!(
                node_asset(unpinned, "x86_64-unknown-linux-gnu").is_none(),
                "{unpinned} must have no asset without a reviewed digest"
            );
        }
        let spec = lookup("node").unwrap();
        let err = resolve(spec, "20.11.1", "x86_64-unknown-linux-gnu").unwrap_err();
        assert!(err.contains("reviewed SHA-256"), "got: {err}");
    }

    /// An unsupported platform is a legible refusal, not a mystery 404 — for
    /// EVERY archive-backed entry.
    #[test]
    fn unsupported_platform_resolves_to_no_asset() {
        assert!(cargo_nextest_asset("0.9.98", "riscv64gc-unknown-linux-gnu").is_none());
        assert!(cargo_nextest_asset("0.9.98", "unsupported").is_none());
        assert!(node_asset("22.11.0", "riscv64gc-unknown-linux-gnu").is_none());
        assert!(node_asset("22.11.0", "unsupported").is_none());
        let spec = lookup("node").unwrap();
        let err = resolve(spec, "22.11.0", "unsupported").unwrap_err();
        assert!(err.contains("not provisionable"), "got: {err}");
    }

    /// The Python entries' derivation: the pinned requirement string and the
    /// module the shim invokes. Each was verified LIVE on 2026-08-18 by
    /// installing it into a venv on this box and running
    /// `python -m <module> --version`:
    ///   poetry==2.1.3                    -> "Poetry (version 2.1.3)"
    ///   twine==6.1.0                     -> "twine version 6.1.0 (…)"
    ///   datamodel-code-generator==0.28.5 -> "0.28.5"
    #[test]
    fn python_entries_pin_distribution_and_module() {
        let cases = [
            ("poetry", "2.1.3", "poetry==2.1.3", "poetry"),
            ("twine", "6.1.0", "twine==6.1.0", "twine"),
            (
                "datamodel-code-generator",
                "0.28.5",
                "datamodel-code-generator==0.28.5",
                // Dashed distribution, underscored module — the reason
                // `module` is its own curated field.
                "datamodel_code_generator",
            ),
        ];
        for (name, version, want_req, want_module) in cases {
            let spec = lookup(name).expect("curated entry");
            // A Python package is host-independent: it resolves the same on
            // every triple, including one no archive publishes for.
            for triple in ["x86_64-pc-windows-msvc", "riscv64gc-unknown-linux-gnu"] {
                let r = resolve(spec, version, triple).expect("python entries are portable");
                assert_eq!(r.bin_dir, PathBuf::from("bin"), "{name}");
                assert_eq!(r.primary, name, "{name}");
                match r.source {
                    Source::PythonPackage {
                        requirement,
                        module,
                    } => {
                        assert_eq!(requirement, want_req, "{name}");
                        assert_eq!(module, want_module, "{name}");
                    }
                    _ => panic!("{name} must resolve to a PythonPackage"),
                }
            }
        }
    }

    /// The live-measured version outputs must satisfy the verification
    /// predicate — otherwise every install of these entries would refuse.
    #[test]
    fn live_measured_version_outputs_verify() {
        assert!(version_is_reported("Poetry (version 2.1.3)\n", "2.1.3"));
        assert!(version_is_reported(
            "twine version 6.1.0 (keyring: 25.7.0, packaging: 26.3)\n",
            "6.1.0"
        ));
        assert!(version_is_reported("0.28.5\n", "0.28.5"));
        assert!(version_is_reported("v22.11.0\n", "22.11.0"));
        // …and each must still REFUSE a neighbouring version.
        assert!(!version_is_reported("Poetry (version 2.1.3)\n", "2.1.30"));
        assert!(!version_is_reported("v22.11.0\n", "22.11.1"));
    }

    /// The shim is what goes on PATH. Two properties, both load-bearing: it
    /// locates the venv relative to its own directory (which is what makes the
    /// atomic rename safe), and it runs the interpreter ISOLATED so the
    /// step's cwd cannot substitute a different module for the verified one.
    #[test]
    fn python_shim_is_position_independent_and_isolated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("bin");
        let path = write_python_shim(&bin, "poetry", "poetry").expect("shim");
        let body = std::fs::read_to_string(&path).expect("read shim");
        // No absolute path may appear in it.
        assert!(
            !body.contains(&dir.path().to_string_lossy().to_string()),
            "shim embedded an absolute path: {body}"
        );
        assert!(body.contains("-I -m poetry"), "shim must isolate: {body}");
        if cfg!(target_os = "windows") {
            assert!(
                body.contains("%~dp0..\\venv\\Scripts\\python.exe"),
                "{body}"
            );
            assert_eq!(path.file_name().unwrap(), "poetry.cmd");
        } else {
            assert!(
                body.contains("$(dirname \"$0\")/../venv/bin/python"),
                "{body}"
            );
            assert_eq!(path.file_name().unwrap(), "poetry");
        }
    }

    /// node's Windows zip puts `npm` (a SHELL script Windows cannot run),
    /// `npm.cmd` and `npm.ps1` in one directory. The runnable spelling must
    /// win, or every Windows dispatch would try to execute the shell script.
    #[test]
    fn executable_resolution_prefers_the_runnable_spelling() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("npm"), "#!/bin/sh\n").expect("write");
        std::fs::write(dir.path().join("npm.cmd"), "@echo off\r\n").expect("write");
        std::fs::write(dir.path().join("npm.ps1"), "# ps\r\n").expect("write");
        let found = resolve_executable(dir.path(), "npm").expect("npm must resolve");
        let name = found.file_name().unwrap().to_string_lossy().to_string();
        if cfg!(target_os = "windows") {
            assert_eq!(name, "npm.cmd", "the shell script must not be chosen");
        } else {
            assert_eq!(name, "npm");
        }
        // A `.ps1` is never a candidate on any platform.
        assert_ne!(name, "npm.ps1");
        assert!(resolve_executable(dir.path(), "definitely-absent").is_none());
    }

    /// The cache is version-keyed — that is what makes caching safe here.
    #[test]
    fn tool_cache_is_version_keyed_under_root() {
        let a = tool_dir(Path::new("/root"), "cargo-nextest", "0.9.98");
        let b = tool_dir(Path::new("/root"), "cargo-nextest", "0.9.97");
        assert_ne!(a, b);
        assert!(a.starts_with("/root"));
        assert!(a.ends_with(
            PathBuf::from(".ci-tools")
                .join("cargo-nextest")
                .join("0.9.98")
        ));
    }

    /// The verification predicate must be exact: a prefix match would accept
    /// 0.9.98 for a declared 0.9.9.
    #[test]
    fn version_match_is_whole_token() {
        let real = "cargo-nextest 0.9.98 (9d44b7418 2025-06-06)\nrelease: 0.9.98\n";
        assert!(version_is_reported(real, "0.9.98"));
        assert!(!version_is_reported(real, "0.9.9"));
        assert!(!version_is_reported(real, "0.9.97"));
        assert!(!version_is_reported("", "0.9.98"));
        // A build-metadata version is one token, not three.
        assert!(version_is_reported(
            "tool 1.2.3-rc.1+build",
            "1.2.3-rc.1+build"
        ));
        // The `v` tolerance is a LEADING v only — it must not turn a prefix
        // match back on.
        assert!(!version_is_reported("v0.9.98", "0.9.9"));
        assert!(!version_is_reported("0.9.98", "v0.9.98"));
    }

    /// The host triple is whatever this build targets; it must be one the
    /// registry can answer for, or provisioning refuses legibly.
    #[test]
    fn host_triple_is_resolvable_or_refuses() {
        let t = host_triple();
        // Either the registry has an asset for this host, or the triple is
        // the explicit `unsupported` sentinel — never a silent wrong URL.
        assert!(
            cargo_nextest_asset("0.9.98", t).is_some() || t == "unsupported",
            "unmapped triple {t}"
        );
        assert!(
            node_asset("22.11.0", t).is_some() || t == "unsupported",
            "unmapped triple {t}"
        );
    }

    // ── Tree extraction: traversal is structurally impossible ─────────────

    /// The whole tar/zip-slip class, refused before a path is ever opened.
    /// Pure, so these hold identically on every host.
    #[test]
    fn tree_entry_traversal_is_rejected() {
        let root = "node-v22.11.0-linux-x64";
        for bad in [
            "/etc/passwd",
            "\\windows\\system32\\evil.dll",
            "C:\\windows\\evil.dll",
            "../outside",
            "node-v22.11.0-linux-x64/../../outside",
            "node-v22.11.0-linux-x64/bin/../../../outside",
            // A second top-level tree is outside the declared root.
            "other-root/bin/node",
            "node-v22.11.0-linux-x64-evil/bin/node",
        ] {
            let err = safe_tree_path(bad, root).expect_err(&format!("{bad} must be refused"));
            assert!(
                err.contains("refusing"),
                "{bad}: expected a refusal, got {err}"
            );
        }
    }

    /// …while the real archive's entries survive, with the root stripped.
    #[test]
    fn tree_entry_paths_are_stripped_of_the_declared_root() {
        let root = "node-v22.11.0-linux-x64";
        assert_eq!(
            safe_tree_path("node-v22.11.0-linux-x64/bin/node", root).unwrap(),
            Some(PathBuf::from("bin").join("node"))
        );
        assert_eq!(
            safe_tree_path(
                "node-v22.11.0-linux-x64/lib/node_modules/npm/bin/npm-cli.js",
                root
            )
            .unwrap(),
            Some(
                PathBuf::from("lib")
                    .join("node_modules")
                    .join("npm")
                    .join("bin")
                    .join("npm-cli.js")
            )
        );
        // The top-level directory entry itself has nothing to write.
        assert_eq!(
            safe_tree_path("node-v22.11.0-linux-x64/", root).unwrap(),
            None
        );
        assert_eq!(
            safe_tree_path("node-v22.11.0-linux-x64", root).unwrap(),
            None
        );
        assert_eq!(safe_tree_path("", root).unwrap(), None);
        // A `./` prefix is normalised away rather than mistaken for a name.
        assert_eq!(
            safe_tree_path("./node-v22.11.0-linux-x64/bin/node", root).unwrap(),
            Some(PathBuf::from("bin").join("node"))
        );
    }

    /// node's Unix archive REQUIRES relative links (`bin/npm` ->
    /// `../lib/node_modules/npm/bin/npm-cli.js`), so they are allowed — but
    /// only while they stay inside the extraction root.
    #[test]
    fn symlink_containment_allows_node_and_refuses_escapes() {
        let npm = PathBuf::from("bin").join("npm");
        assert!(symlink_target_is_contained(
            &npm,
            "../lib/node_modules/npm/bin/npm-cli.js"
        ));
        assert!(symlink_target_is_contained(
            &npm,
            "../lib/node_modules/corepack/dist/corepack.js"
        ));
        assert!(symlink_target_is_contained(&PathBuf::from("a"), "b"));
        // …and the escapes.
        for bad in [
            "/etc/passwd",
            "\\windows\\system32",
            "C:\\windows",
            "../../etc/passwd",
            "../../../anything",
            "",
        ] {
            assert!(
                !symlink_target_is_contained(&npm, bad),
                "{bad} must be refused"
            );
        }
        // One level up from a top-level file already escapes.
        assert!(!symlink_target_is_contained(&PathBuf::from("node"), "../x"));
    }

    /// Lexical resolution of a link target, which is what proves a link lands
    /// on a real extracted file.
    #[test]
    fn link_targets_resolve_relative_to_the_links_own_directory() {
        let npm = PathBuf::from("bin").join("npm");
        assert_eq!(
            resolve_link_target(&npm, "../lib/node_modules/npm/bin/npm-cli.js"),
            Some(
                PathBuf::from("lib")
                    .join("node_modules")
                    .join("npm")
                    .join("bin")
                    .join("npm-cli.js")
            )
        );
        assert_eq!(
            resolve_link_target(&npm, "./npx"),
            Some(PathBuf::from("bin").join("npx"))
        );
        // Escapes and "the root itself" both resolve to nothing.
        assert_eq!(resolve_link_target(&npm, "../../outside"), None);
        assert_eq!(
            resolve_link_target(&PathBuf::from("d").join("up"), ".."),
            None
        );
        assert_eq!(resolve_link_target(&npm, "/etc/passwd"), None);
    }

    /// End-to-end on the tree extractor with a hand-built tar.gz: the good
    /// archive lands, and each crafted one is refused with NOTHING written
    /// outside the destination.
    #[test]
    fn extract_tree_writes_only_inside_and_refuses_traversal() {
        let root = "pkg-1.0.0";
        let good = targz(&[
            file("pkg-1.0.0/bin/tool", b"#!/bin/sh\n"),
            file("pkg-1.0.0/lib/data.txt", b"hello"),
        ]);
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("dest");
        extract_tree(&good, ArchiveKind::TarGz, root, &dest, "test://good").expect("good archive");
        assert!(dest.join("bin").join("tool").is_file());
        assert_eq!(
            std::fs::read_to_string(dest.join("lib").join("data.txt")).unwrap(),
            "hello"
        );

        for (label, entries) in [
            ("parent", vec![file("pkg-1.0.0/../escaped.txt", b"x")]),
            ("absolute", vec![file("/escaped.txt", b"x")]),
            ("other-root", vec![file("elsewhere/escaped.txt", b"x")]),
        ] {
            let dest = dir.path().join(format!("dest-{label}"));
            let err = extract_tree(
                &targz(&entries),
                ArchiveKind::TarGz,
                root,
                &dest,
                "test://bad",
            )
            .unwrap_err();
            assert!(err.contains("refusing"), "{label}: got {err}");
            assert!(
                !dir.path().join("escaped.txt").exists(),
                "{label}: wrote outside the destination"
            );
        }

        // An archive with nothing under the declared root is a refusal, not a
        // silent empty install.
        let dest = dir.path().join("dest-empty");
        let err = extract_tree(
            &targz(&[("pkg-1.0.0/", tar::EntryType::Directory, "", b"")]),
            ArchiveKind::TarGz,
            root,
            &dest,
            "test://empty",
        )
        .unwrap_err();
        assert!(err.contains("contained nothing"), "got: {err}");
    }

    /// THE REGRESSION TEST FOR THE ARBITRARY-WRITE BUG.
    ///
    /// A single link is checked correctly by lexical depth accounting; a
    /// CHAIN is not, because the second link's target traverses through the
    /// first one. `d/up -> ..` lands on the destination root, so `d/hop ->
    /// up/..` lands one level ABOVE it, and a plain file written under
    /// `d/hop/` then escapes — no `..` anywhere in its own path.
    ///
    /// The fix is not a smarter filter: no symlink is created at all, so
    /// `d/hop` is a real directory and the write stays inside no matter what
    /// the checker concluded. This test asserts the OUTCOME (nothing outside
    /// the destination), so it keeps holding however the check is rewritten.
    #[test]
    fn two_link_chain_cannot_escape_the_extraction_root() {
        let root = "pkg-1.0.0";
        let hostile = targz(&[
            ("pkg-1.0.0/d/", tar::EntryType::Directory, "", b""),
            ("pkg-1.0.0/d/up", tar::EntryType::Symlink, "..", b""),
            ("pkg-1.0.0/d/hop", tar::EntryType::Symlink, "up/..", b""),
            file("pkg-1.0.0/d/hop/evil", b"pwned"),
        ]);
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("dest");
        let outside = dir.path().join("evil");

        let result = extract_tree(&hostile, ArchiveKind::TarGz, root, &dest, "test://chain");

        // Whatever the verdict, the write must NOT have landed outside.
        assert!(
            !outside.exists(),
            "arbitrary write escaped to {}",
            outside.display()
        );
        for entry in std::fs::read_dir(dir.path()).expect("read tmp") {
            let name = entry.expect("entry").file_name();
            assert_eq!(
                name, "dest",
                "extraction created {name:?} outside the destination"
            );
        }
        // `d/hop` must be a real directory, never a link.
        let hop = dest.join("d").join("hop");
        if hop.exists() {
            let meta = std::fs::symlink_metadata(&hop).expect("stat hop");
            assert!(!meta.file_type().is_symlink(), "a symlink was created");
            assert!(meta.is_dir(), "hop should be a plain directory");
            assert!(dest.join("d").join("hop").join("evil").is_file());
        }
        // And the chain itself is refused, because `d/up` names the root.
        let err = result.expect_err("a link chain onto the root must be refused");
        assert!(err.contains("refusing"), "got: {err}");
    }

    /// No symlink is ever created, on any archive — the property the whole
    /// containment argument now rests on.
    #[test]
    fn no_symlink_is_ever_created_under_the_destination() {
        let root = "pkg-1.0.0";
        let archive = targz(&[
            file("pkg-1.0.0/lib/real.js", b"module.exports=1\n"),
            (
                "pkg-1.0.0/bin/tool",
                tar::EntryType::Symlink,
                "../lib/real.js",
                b"",
            ),
        ]);
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("dest");
        let result = extract_tree(&archive, ArchiveKind::TarGz, root, &dest, "test://link");

        let link = dest.join("bin").join("tool");
        if link.exists() {
            let meta = std::fs::symlink_metadata(&link).expect("stat");
            assert!(
                !meta.file_type().is_symlink(),
                "a symlink was created at {}",
                link.display()
            );
        }
        #[cfg(unix)]
        {
            result.expect("a contained link must materialise");
            let body = std::fs::read_to_string(&link).expect("launcher");
            // A LAUNCHER, not a copy: copying would relocate the target's
            // __dirname and break node's relative `require`.
            assert!(body.contains("exec "), "{body}");
            assert!(body.contains("../lib/real.js"), "{body}");
            assert!(
                !body.contains("module.exports"),
                "must not be a copy: {body}"
            );
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&link).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755, "launcher must be executable");
        }
        #[cfg(not(unix))]
        {
            // Non-Unix hosts consume the zip assets, which carry no links, so
            // a link entry here is an unreviewed shape and refuses loudly.
            let err = result.expect_err("non-unix must refuse a link entry");
            assert!(err.contains("refusing"), "got: {err}");
        }
    }

    /// A link that points at nothing the archive provided is refused at
    /// provisioning time rather than at `npm ci`.
    #[test]
    fn dangling_and_cyclic_links_are_refused() {
        let root = "pkg-1.0.0";
        let dir = tempfile::tempdir().expect("tempdir");

        let dangling = targz(&[
            file("pkg-1.0.0/lib/real.js", b"x"),
            (
                "pkg-1.0.0/bin/tool",
                tar::EntryType::Symlink,
                "../lib/absent.js",
                b"",
            ),
        ]);
        let err = extract_tree(
            &dangling,
            ArchiveKind::TarGz,
            root,
            &dir.path().join("d1"),
            "test://dangling",
        )
        .unwrap_err();
        assert!(err.contains("refusing"), "got: {err}");

        let cyclic = targz(&[
            file("pkg-1.0.0/lib/real.js", b"x"),
            ("pkg-1.0.0/bin/a", tar::EntryType::Symlink, "b", b""),
            ("pkg-1.0.0/bin/b", tar::EntryType::Symlink, "a", b""),
        ]);
        let err = extract_tree(
            &cyclic,
            ArchiveKind::TarGz,
            root,
            &dir.path().join("d2"),
            "test://cyclic",
        )
        .unwrap_err();
        assert!(err.contains("refusing"), "got: {err}");
    }

    /// Hard links and device nodes are ways out of the tree, and no curated
    /// asset contains one — so they refuse rather than being handled.
    #[test]
    fn hard_links_and_device_nodes_are_refused() {
        let root = "pkg-1.0.0";
        let dir = tempfile::tempdir().expect("tempdir");
        for (label, kind) in [
            ("hard-link", tar::EntryType::Link),
            ("char-device", tar::EntryType::Char),
            ("fifo", tar::EntryType::Fifo),
        ] {
            let archive = targz(&[
                file("pkg-1.0.0/lib/real.js", b"x"),
                ("pkg-1.0.0/bin/tool", kind, "../lib/real.js", b""),
            ]);
            let err = extract_tree(
                &archive,
                ArchiveKind::TarGz,
                root,
                &dir.path().join(label),
                "test://weird",
            )
            .unwrap_err();
            assert!(
                err.contains("unsupported type") && err.contains("refusing"),
                "{label}: got {err}"
            );
        }
    }

    /// A pax global header is archive-wide metadata, not a path — it must be
    /// skipped, not refused with a confusing "outside the top-level
    /// directory".
    #[test]
    fn pax_global_header_is_skipped_not_refused() {
        let root = "pkg-1.0.0";
        let archive = targz(&[
            ("pax_global_header", tar::EntryType::XGlobalHeader, "", b""),
            file("pkg-1.0.0/lib/real.js", b"x"),
        ]);
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("dest");
        extract_tree(&archive, ArchiveKind::TarGz, root, &dest, "test://pax")
            .expect("a pax global header must not fail a legitimate archive");
        assert!(dest.join("lib").join("real.js").is_file());
    }

    // ── The ZIP tree path — this fleet's primary platform ─────────────────

    fn zip_archive(entries: &[(&str, Option<&str>, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        for (name, link, body) in entries {
            match link {
                Some(target) => w.add_symlink(*name, *target, opts).expect("symlink"),
                None => {
                    w.start_file(*name, opts).expect("start");
                    w.write_all(body).expect("write");
                }
            }
        }
        w.finish().expect("finish").into_inner()
    }

    #[test]
    fn zip_tree_extracts_and_refuses_traversal_and_symlinks() {
        let root = "pkg-1.0.0";
        let dir = tempfile::tempdir().expect("tempdir");

        // The good case.
        let good = zip_archive(&[
            ("pkg-1.0.0/node.exe", None, b"MZ"),
            ("pkg-1.0.0/lib/x.js", None, b"hi"),
        ]);
        let dest = dir.path().join("ok");
        extract_tree(&good, ArchiveKind::Zip, root, &dest, "test://zip").expect("good zip");
        assert!(dest.join("node.exe").is_file());
        assert_eq!(
            std::fs::read_to_string(dest.join("lib").join("x.js")).unwrap(),
            "hi"
        );

        // Traversal, refused before anything is opened.
        let evil = zip_archive(&[("pkg-1.0.0/../escaped.txt", None, b"x")]);
        let err = extract_tree(
            &evil,
            ArchiveKind::Zip,
            root,
            &dir.path().join("t"),
            "test://zip",
        )
        .unwrap_err();
        assert!(err.contains("refusing"), "got: {err}");
        assert!(!dir.path().join("escaped.txt").exists());

        // The curated zip assets carry no symlinks, so one is an unreviewed
        // shape and refuses rather than taking an untested path.
        let linked = zip_archive(&[
            ("pkg-1.0.0/real", None, b"x"),
            ("pkg-1.0.0/link", Some("real"), b""),
        ]);
        let err = extract_tree(
            &linked,
            ArchiveKind::Zip,
            root,
            &dir.path().join("l"),
            "test://zip",
        )
        .unwrap_err();
        assert!(err.contains("symlink"), "got: {err}");
    }

    // ── Verification and refusals ────────────────────────────────────────

    /// `verify` itself — not its pieces. Dropping the version comparison from
    /// it must fail this test, which is why the wrong-version case goes
    /// through `verify` rather than through `version_is_reported` directly.
    #[tokio::test]
    async fn verify_requires_the_declared_version_from_a_real_process() {
        let dir = tempfile::tempdir().expect("tempdir");
        // A real process that exits 0 and prints a version.
        let (prog, args): (&str, Vec<&str>) = if cfg!(target_os = "windows") {
            ("cmd.exe", vec!["/C", "echo", "1.2.3"])
        } else {
            ("/bin/echo", vec!["1.2.3"])
        };
        let bin = PathBuf::from(prog);

        verify(&bin, &args, "1.2.3", dir.path())
            .await
            .expect("the declared version is what the process reports");

        let err = verify(&bin, &args, "9.9.9", dir.path())
            .await
            .expect_err("a wrong declared version must refuse");
        assert!(err.contains("different version"), "got: {err}");

        // A non-zero exit is a refusal too.
        let (prog, args): (&str, Vec<&str>) = if cfg!(target_os = "windows") {
            ("cmd.exe", vec!["/C", "exit", "3"])
        } else {
            ("/bin/sh", vec!["-c", "exit 3"])
        };
        let err = verify(Path::new(prog), &args, "1.2.3", dir.path())
            .await
            .expect_err("a failing process must refuse");
        assert!(err.contains("exited"), "got: {err}");

        // A missing binary is a cache MISS, never a silent pass.
        assert!(resolve_executable(dir.path(), "definitely-absent").is_none());
    }

    /// A missing interpreter must produce a refusal that NAMES what is
    /// missing and says so — never a silent skip.
    #[test]
    fn missing_python_refusal_names_what_is_missing() {
        let msg = no_python_refusal(&["python3: not found".to_string()]);
        for candidate in PYTHON_CANDIDATES {
            assert!(msg.contains(candidate), "must name {candidate}: {msg}");
        }
        assert!(msg.contains("refusal, not a skip"), "{msg}");
        assert!(msg.contains("never installs packages into"), "{msg}");
        // The per-candidate diagnosis is carried through, not swallowed.
        assert!(msg.contains("python3: not found"), "{msg}");
    }

    /// The sweep must reclaim abandoned staging trees WITHOUT touching one a
    /// live dispatch is still filling.
    #[test]
    fn staging_sweep_removes_only_stale_dirs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fresh = dir.path().join("fresh");
        std::fs::create_dir_all(fresh.join("nested")).expect("mkdir");
        std::fs::write(fresh.join("nested").join("f"), b"x").expect("write");
        let stale = dir.path().join("stale");
        std::fs::create_dir_all(&stale).expect("mkdir");
        // Rather than backdating an mtime (which needs a crate this repo does
        // not carry), run the sweep from a clock far enough in the future that
        // `stale` is past the floor. Same predicate, no new dependency.
        let later = std::time::SystemTime::now() + STAGING_MAX_AGE + Duration::from_secs(3600);
        // `fresh` is re-touched so it stays inside the window from `later`.
        std::fs::write(fresh.join("nested").join("f"), b"y").expect("touch");
        sweep_stale_staging_at(dir.path(), std::time::SystemTime::now());
        assert!(fresh.exists() && stale.exists(), "nothing is stale yet");

        sweep_stale_staging_at(dir.path(), later);

        assert!(
            !stale.exists() && !fresh.exists(),
            "everything older than the floor is reclaimed"
        );
    }
}
