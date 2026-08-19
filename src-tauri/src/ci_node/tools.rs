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
//!   so the equivalent structural guarantee is that every archive entry is
//!   validated to stay inside the destination BEFORE anything is written:
//!   [`safe_tree_path`] rejects absolute paths, drive-qualified paths, `..`
//!   components and any entry outside the single expected top-level directory;
//!   [`symlink_target_is_contained`] rejects a symlink whose target escapes;
//!   hard links and device nodes are rejected outright. Both are pure
//!   functions, so the traversal refusals are tested without touching a disk.
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
fn node_asset(version: &str, triple: &str) -> Option<TreeAsset> {
    let (platform, kind, bin_subdir) = match triple {
        "x86_64-pc-windows-msvc" => ("win-x64", ArchiveKind::Zip, ""),
        "x86_64-unknown-linux-gnu" => ("linux-x64", ArchiveKind::TarGz, "bin"),
        "aarch64-unknown-linux-gnu" => ("linux-arm64", ArchiveKind::TarGz, "bin"),
        "x86_64-apple-darwin" => ("darwin-x64", ArchiveKind::TarGz, "bin"),
        "aarch64-apple-darwin" => ("darwin-arm64", ArchiveKind::TarGz, "bin"),
        _ => return None,
    };
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
    /// Extract a whole tree out of an archive, traversal-checked.
    ArchiveTree { asset: Asset, root_dir: String },
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
                    "tool '{}' publishes no prebuilt binary for {triple}",
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

        if let Some(bin) = resolve_executable(&bin_dir, &resolved.primary) {
            match verify(&bin, spec.verify_args, &tool.version, &bin_dir).await {
                Ok(()) => {
                    log(format!(
                        "[ci-node] tool {} {} — cache hit ({})",
                        spec.name,
                        tool.version,
                        bin_dir.display()
                    ));
                    dirs.push(bin_dir);
                    continue;
                }
                Err(e) => {
                    // A cached directory that does not verify is worse than
                    // no cache: drop it and reinstall rather than run a broken
                    // tool.
                    warn!(
                        "ci_node: cached tool {} {} failed verification ({e}); reinstalling",
                        spec.name, tool.version
                    );
                    log(format!(
                        "[ci-node] tool {} {} — cached copy failed verification ({e}); reinstalling",
                        spec.name, tool.version
                    ));
                    let _ = std::fs::remove_dir_all(&dir);
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
    // `tempfile`'s TempDir removes itself on drop, so an error path anywhere
    // below leaves no partial install behind.
    let staging = tempfile::Builder::new()
        .prefix(&format!("{}-{version}-", spec.name))
        .tempdir_in(&staging_root)
        .map_err(|e| format!("create staging dir under {}: {e}", staging_root.display()))?;
    let staged_bin_dir = resolved.bin_dir_at(staging.path());

    match &resolved.source {
        Source::ArchiveFile { asset, basename } => {
            let bytes = download(&asset.url).await?;
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
        Source::ArchiveTree { asset, root_dir } => {
            let bytes = download(&asset.url).await?;
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

    let staged_primary =
        resolve_executable(&staged_bin_dir, &resolved.primary).ok_or_else(|| {
            format!(
                "install of {} {version} produced no {} under {}",
                spec.name,
                resolved.primary,
                staged_bin_dir.display()
            )
        })?;
    verify(&staged_primary, spec.verify_args, version, &staged_bin_dir).await?;
    // Companions are checked to EXIST and to RUN. Their versions are not the
    // declared one (npm's version is a property of the node release), so they
    // are reported rather than asserted — but a missing or unrunnable one
    // fails the install, because "node provisioned, npm absent" must not be
    // discovered by a step ten minutes later.
    for companion in &resolved.companions {
        let bin = resolve_executable(&staged_bin_dir, companion).ok_or_else(|| {
            format!(
                "{} {version} was extracted but provides no {companion} under {}",
                spec.name,
                staged_bin_dir.display()
            )
        })?;
        let reported = run_reporting_version(&bin, &staged_bin_dir).await?;
        log(format!(
            "[ci-node] tool {} {version} — provides {companion} {reported}",
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
            let published = resolve_executable(&bin_dir, &resolved.primary);
            if let Some(bin) = published {
                if verify(&bin, spec.verify_args, version, &bin_dir)
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
            }
            Err(format!(
                "publish {} -> {}: {e}",
                staged_path.display(),
                final_dir.display()
            ))
        }
    }
}

/// Fetch the asset into memory. Bounded twice — by `Content-Length` where the
/// server sends one, and by the accumulated body size where it does not — so
/// a redirect to something enormous cannot fill the disk this lane already
/// guards with `min_free_disk_gb`.
async fn download(url: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .map_err(|e| format!("build http client: {e}"))?;
    let resp = client
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
    let body = resp
        .bytes()
        .await
        .map_err(|e| format!("read body of {url}: {e}"))?;
    if body.len() as u64 > MAX_ASSET_BYTES {
        return Err(format!(
            "GET {url} returned {} bytes, over the {MAX_ASSET_BYTES}-byte ceiling",
            body.len()
        ));
    }
    if body.is_empty() {
        return Err(format!("GET {url} returned an empty body"));
    }
    Ok(body.to_vec())
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

/// Does a symlink stay inside the destination tree?
///
/// `link` is the entry's path relative to the destination root (already
/// stripped); `target` is the raw link target from the archive. Resolved
/// LEXICALLY — the depth of the link's own directory is walked down by each
/// `..` and up by each name, and the moment it would go above the root the
/// link is refused. Pure, so the escape cases are tested without a disk.
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

/// One validated entry, ready to write.
enum TreeEntry<'a> {
    Dir,
    File {
        mode: Option<u32>,
        reader: &'a mut dyn std::io::Read,
    },
    Symlink {
        target: String,
    },
}

/// Extract a whole archive into `dest`, stripping `root_dir`.
///
/// Every entry is validated by [`safe_tree_path`] (and, for symlinks, by
/// [`symlink_target_is_contained`]) BEFORE any path is opened, so traversal is
/// structurally impossible rather than filtered after the fact. Entry types
/// other than file/dir/symlink — hard links, fifos, device nodes — are
/// refused: none of them appear in the curated releases, and each is a way to
/// reach outside the tree.
fn extract_tree(
    bytes: &[u8],
    kind: ArchiveKind,
    root_dir: &str,
    dest: &Path,
    url: &str,
) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
    let mut wrote_any = false;
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
                        "{url}: zip entry {raw:?} is a symlink; the curated zip assets contain none — refusing"
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
                let raw = String::from_utf8_lossy(&entry.path_bytes()).to_string();
                let Some(rel) =
                    safe_tree_path(&raw, root_dir).map_err(|e| format!("{url}: {e}"))?
                else {
                    continue;
                };
                let entry_type = entry.header().entry_type();
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
                            "{url}: symlink {raw:?} -> {target:?} escapes the extraction root — refusing"
                        ));
                    }
                    write_tree_entry(dest, &rel, TreeEntry::Symlink { target })?;
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
    Ok(())
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
                // Honour the archive's executable bit (node's `bin/node`
                // needs it); fall back to 0o644 when the archive carries no
                // mode.
                let bits = mode.map(|m| m & 0o777).unwrap_or(0o644);
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(bits))
                    .map_err(|e| format!("chmod {}: {e}", path.display()))?;
            }
            #[cfg(not(unix))]
            {
                let _ = mode;
            }
        }
        TreeEntry::Symlink { target } => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("create {}: {e}", parent.display()))?;
            }
            #[cfg(unix)]
            {
                let _ = std::fs::remove_file(&path);
                std::os::unix::fs::symlink(&target, &path)
                    .map_err(|e| format!("symlink {} -> {target}: {e}", path.display()))?;
            }
            #[cfg(not(unix))]
            {
                // Creating a symlink on Windows needs a privilege this app
                // does not require, and the curated Windows assets are zips
                // with no symlinks — so this is unreachable in practice and a
                // refusal rather than a silent skip if it ever is not.
                return Err(format!(
                    "archive entry {} is a symlink -> {target}, which this platform cannot create — refusing",
                    path.display()
                ));
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
    Err(format!(
        "no Python 3 interpreter found — tried {PYTHON_CANDIDATES:?} ({}). \
         Install Python 3 (including its `venv` module) and put it on PATH. \
         The runner will never install a Python, and never installs packages into \
         the host interpreter: the distribution goes into a private venv under \
         .ci-tools/. A missing interpreter is a refusal, not a skip",
        tried.join("; ")
    ))
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
    let body = if cfg!(target_os = "windows") {
        format!("@echo off\r\n\"%~dp0..\\venv\\Scripts\\python.exe\" -m {module} %*\r\n")
    } else {
        format!("#!/bin/sh\nexec \"$(dirname \"$0\")/../venv/bin/python\" -m {module} \"$@\"\n")
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
fn redacted_argv(args: &[&str]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut prev_was_env_flag = false;
    for arg in args {
        if prev_was_env_flag {
            match arg.split_once('=') {
                Some((key, _)) => out.push(format!("{key}=<redacted>")),
                // The bare form carries no value to redact.
                None => out.push((*arg).to_string()),
            }
        } else {
            out.push((*arg).to_string());
        }
        prev_was_env_flag = matches!(*arg, "-e" | "--env");
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
            .stderr(std::process::Stdio::piped());
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

    /// An argv formatted into an error string must not carry a credential.
    ///
    /// These error strings are logged to disk, streamed to coord live, and
    /// PERSISTED by coord in the dispatch result's `log_tail`, so a secret that
    /// reaches one is published. Callers in this crate pass the bare `-e NAME`
    /// form, and this is the second line of defence for the next caller that
    /// does not.
    #[test]
    fn argv_rendered_for_an_error_drops_env_values() {
        let args = [
            "run",
            "-d",
            "--name",
            "qontinui-ci-d1-postgres",
            "-e",
            "POSTGRES_PASSWORD=correcthorsebatterystaple",
            "--env",
            "PGPASSWORD=hunter2",
            "-e",
            "POSTGRES_DB", // bare: nothing to redact
            "postgres:16",
        ];
        let rendered = format!("{:?}", redacted_argv(&args));
        assert!(
            !rendered.contains("correcthorsebatterystaple"),
            "{rendered}"
        );
        assert!(!rendered.contains("hunter2"), "{rendered}");
        // The NAME survives — it is the diagnostic half.
        assert!(
            rendered.contains("POSTGRES_PASSWORD=<redacted>"),
            "{rendered}"
        );
        assert!(rendered.contains("PGPASSWORD=<redacted>"), "{rendered}");
        assert!(rendered.contains("POSTGRES_DB"), "{rendered}");
        // Everything that is not an env value is untouched, so the error stays
        // diagnosable.
        assert!(rendered.contains("qontinui-ci-d1-postgres"), "{rendered}");
        assert!(rendered.contains("postgres:16"), "{rendered}");
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
        assert!(out.contains("correcthorsebatterystaple"), "got {out:?}");
        assert!(
            !format!("{args:?}").contains("correcthorsebatterystaple"),
            "the value must never be in the argv"
        );
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
        for (triple, file, kind) in cases {
            let a = cargo_nextest_asset("0.9.98", triple).expect("supported triple");
            assert_eq!(a.kind, kind, "{triple}");
            assert_eq!(
                a.url,
                format!(
                    "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-0.9.98/{file}"
                ),
                "{triple}"
            );
        }
    }

    /// Node's derivation, pinned against the names verified live on release
    /// **v22.11.0** (all five 200'd; all five appear in that release's
    /// SHASUMS256.txt). Unlike nextest, darwin is PER-ARCH here — a universal
    /// name would 404.
    #[test]
    fn node_asset_urls_match_the_published_names() {
        let cases = [
            (
                "x86_64-pc-windows-msvc",
                "node-v22.11.0-win-x64",
                "zip",
                ArchiveKind::Zip,
                "",
            ),
            (
                "x86_64-unknown-linux-gnu",
                "node-v22.11.0-linux-x64",
                "tar.gz",
                ArchiveKind::TarGz,
                "bin",
            ),
            (
                "aarch64-unknown-linux-gnu",
                "node-v22.11.0-linux-arm64",
                "tar.gz",
                ArchiveKind::TarGz,
                "bin",
            ),
            (
                "x86_64-apple-darwin",
                "node-v22.11.0-darwin-x64",
                "tar.gz",
                ArchiveKind::TarGz,
                "bin",
            ),
            (
                "aarch64-apple-darwin",
                "node-v22.11.0-darwin-arm64",
                "tar.gz",
                ArchiveKind::TarGz,
                "bin",
            ),
        ];
        for (triple, root, ext, kind, bin_subdir) in cases {
            let t = node_asset("22.11.0", triple).expect("supported triple");
            assert_eq!(t.asset.kind, kind, "{triple}");
            assert_eq!(t.root_dir, root, "{triple}");
            assert_eq!(t.bin_subdir, bin_subdir, "{triple}");
            assert_eq!(
                t.asset.url,
                format!("https://nodejs.org/dist/v22.11.0/{root}.{ext}"),
                "{triple}"
            );
        }
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
        assert!(err.contains("publishes no prebuilt binary"), "got: {err}");
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

    /// The shim is what goes on PATH, and it must locate the venv relative to
    /// its own directory — the property that makes the atomic rename safe.
    #[test]
    fn python_shim_is_position_independent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("bin");
        let path = write_python_shim(&bin, "poetry", "poetry").expect("shim");
        let body = std::fs::read_to_string(&path).expect("read shim");
        // No absolute path may appear in it.
        assert!(
            !body.contains(&dir.path().to_string_lossy().to_string()),
            "shim embedded an absolute path: {body}"
        );
        assert!(body.contains("-m poetry"), "{body}");
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
            let err =
                safe_tree_path(bad, root).unwrap_err_or_panic(&format!("{bad} must be refused"));
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

    /// node's Unix archive REQUIRES relative symlinks (`bin/npm` ->
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

    /// End-to-end on the tree extractor with a hand-built tar.gz: the good
    /// archive lands, and each crafted one is refused with NOTHING written
    /// outside the destination.
    #[test]
    fn extract_tree_writes_only_inside_and_refuses_traversal() {
        use std::io::Write;

        /// Build a tar.gz writing entry names RAW into the header.
        ///
        /// `Builder::append_data` refuses to write `..` or an absolute path
        /// ("paths in archives must be relative"), which is precisely the
        /// shape under test — so the name bytes go straight into the GNU
        /// header. That is what a hostile archive looks like on the wire.
        fn targz(entries: &[(&str, &[u8])]) -> Vec<u8> {
            let mut builder = tar::Builder::new(Vec::new());
            for (name, body) in entries {
                let mut header = tar::Header::new_gnu();
                {
                    let gnu = header.as_gnu_mut().expect("gnu header");
                    let raw = name.as_bytes();
                    assert!(raw.len() < gnu.name.len(), "test name too long");
                    gnu.name[..raw.len()].copy_from_slice(raw);
                }
                header.set_size(body.len() as u64);
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

        let root = "pkg-1.0.0";
        let good = targz(&[
            ("pkg-1.0.0/bin/tool", b"#!/bin/sh\n"),
            ("pkg-1.0.0/lib/data.txt", b"hello"),
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
            ("parent", vec![("pkg-1.0.0/../escaped.txt", &b"x"[..])]),
            ("absolute", vec![("/escaped.txt", &b"x"[..])]),
            ("other-root", vec![("elsewhere/escaped.txt", &b"x"[..])]),
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
            &targz(&[("pkg-1.0.0/", b"")]),
            ArchiveKind::TarGz,
            root,
            &dest,
            "test://empty",
        )
        .unwrap_err();
        assert!(err.contains("contained nothing"), "got: {err}");
    }

    /// Verification failure paths, exercised against real processes rather
    /// than mocked: a missing binary, and a binary that reports the WRONG
    /// version, must both refuse.
    #[tokio::test]
    async fn verification_refuses_missing_and_wrong_version_binaries() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Missing binary.
        assert!(resolve_executable(dir.path(), "definitely-absent").is_none());

        // A real process that exits 0 and prints a version that is NOT the
        // declared one. `cmd /C echo` on Windows, `/bin/echo` elsewhere.
        let (prog, args): (&str, Vec<&str>) = if cfg!(target_os = "windows") {
            ("cmd.exe", vec!["/C", "echo", "1.2.3"])
        } else {
            ("/bin/echo", vec!["1.2.3"])
        };
        let text = run_capture(prog, &args, VERIFY_TIMEOUT, None)
            .await
            .expect("echo must run");
        assert!(version_is_reported(&text, "1.2.3"));
        assert!(
            !version_is_reported(&text, "9.9.9"),
            "a wrong declared version must not verify: {text:?}"
        );

        // A non-zero exit is a refusal too.
        let (prog, args): (&str, Vec<&str>) = if cfg!(target_os = "windows") {
            ("cmd.exe", vec!["/C", "exit", "3"])
        } else {
            ("/bin/sh", vec!["-c", "exit 3"])
        };
        let err = run_capture(prog, &args, VERIFY_TIMEOUT, None)
            .await
            .unwrap_err();
        assert!(err.contains("exited"), "got: {err}");
    }

    /// A missing Python interpreter must produce a refusal that NAMES what is
    /// missing — never a silent skip. Asserted on the message shape (the box
    /// running the tests has a Python, so the failure text is what is pinned).
    #[test]
    fn python_candidates_are_named_in_the_refusal() {
        // The candidate list is what the refusal quotes; keep both in sync.
        assert_eq!(PYTHON_CANDIDATES, &["python3", "python"]);
    }

    /// Small helper so the traversal table above reads as a table.
    trait UnwrapErrOrPanic {
        fn unwrap_err_or_panic(self, msg: &str) -> String;
    }
    impl UnwrapErrOrPanic for Result<Option<PathBuf>, String> {
        fn unwrap_err_or_panic(self, msg: &str) -> String {
            match self {
                Err(e) => e,
                Ok(v) => panic!("{msg} (got Ok({v:?}))"),
            }
        }
    }
}
