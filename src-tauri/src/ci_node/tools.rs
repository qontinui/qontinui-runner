//! Toolchain provisioning for CI dispatches: fetch a **prebuilt** binary for
//! a `[[tools]]` declaration, cache it version-pinned, and hand its directory
//! to the executor for the step PATH.
//!
//! **Why this exists.** `ci_node/` installed nothing, so a repo whose Actions
//! gate runs `cargo nextest run` had to substitute plain `cargo test` on this
//! lane. That substitution is not cosmetic: nextest is what writes the JUnit
//! report coord's Tier-7 credibility gate ingests, and without a
//! `test_results` row `test_run_effects.rs::score_for_head` returns `None` and
//! the tier fails CLOSED. coord's own manifest calls that "a HARD BLOCKER for
//! cutover".
//!
//! **Prebuilt, never `cargo install`.** Building cargo-nextest from source
//! "would add many minutes to every dispatch" — on a lane whose whole value is
//! turning a user's idle machine into CI capacity, that is the difference
//! between usable and not. The GitHub Actions lane uses
//! `taiki-e/install-action@v2`, which fetches the project's own release
//! artifact; this module does the same thing against the same artifacts.
//!
//! **The registry is CLOSED.** A manifest names a tool and pins a version; it
//! cannot supply a URL. A repo can already run arbitrary commands as a step,
//! so this is not about what code can execute — it is about not letting a
//! manifest make the RUNNER fetch an arbitrary binary, cache it under a
//! well-known name, and put it on the PATH that later steps resolve
//! implicitly. Adding an entry here is a deliberate, reviewable act.
//!
//! **Caching is safe here in a way sibling caching is not** — the cache key is
//! an exact version, so a hit is by construction the same bytes a miss would
//! fetch. The corruption problem is handled by construction too: a tool
//! directory only ever appears via an atomic rename of a staging directory
//! whose binary has already been executed and shown to report the declared
//! version. A partial download, a truncated archive, a wrong-platform asset
//! and a wrong-version asset all fail that check, and none of them can leave
//! anything at the cache path. On a cache HIT the same verification runs
//! again (it costs one process spawn) so a directory corrupted after the fact
//! — a half-finished antivirus quarantine, a killed dispatch mid-rename on a
//! filesystem without atomic directory rename — self-heals instead of
//! producing a broken tool.

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

/// One entry of the closed registry.
pub(crate) struct ToolSpec {
    pub name: &'static str,
    /// File stem of the binary inside the archive (no `.exe`).
    pub binary: &'static str,
    /// Argv that makes the binary print its version. Checked to exit 0 AND to
    /// mention the declared version.
    pub verify_args: &'static [&'static str],
    /// Resolve the release asset for a version on a given target triple.
    /// `None` means this tool publishes nothing for that platform, which is a
    /// legible refusal rather than a mystery 404.
    pub asset: fn(version: &str, triple: &str) -> Option<Asset>,
}

/// Download ceiling. Prebuilt CLI archives are single-digit MB; anything two
/// orders of magnitude larger is a redirect gone wrong, not a tool.
const MAX_ASSET_BYTES: u64 = 200 * 1024 * 1024;
/// Whole-download budget.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// Budget for the verification spawn.
const VERIFY_TIMEOUT: Duration = Duration::from_secs(60);

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

/// The closed registry.
static KNOWN_TOOLS: &[ToolSpec] = &[ToolSpec {
    name: "cargo-nextest",
    binary: "cargo-nextest",
    // `cargo-nextest --version` and `cargo-nextest nextest --version` both
    // work; the bare form is used because it does not depend on nextest's
    // subcommand layout staying put.
    verify_args: &["--version"],
    asset: cargo_nextest_asset,
}];

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

/// Binary path inside a tool directory.
pub(crate) fn tool_binary_path(dir: &Path, spec: &ToolSpec) -> PathBuf {
    dir.join(format!("{}{}", spec.binary, std::env::consts::EXE_SUFFIX))
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
        let dir = tool_dir(root, spec.name, &tool.version);
        let bin = tool_binary_path(&dir, spec);

        if bin.is_file() {
            match verify(&bin, spec, &tool.version).await {
                Ok(()) => {
                    log(format!(
                        "[ci-node] tool {} {} — cache hit ({})",
                        spec.name,
                        tool.version,
                        dir.display()
                    ));
                    dirs.push(dir);
                    continue;
                }
                Err(e) => {
                    // A cached directory that does not verify is worse than
                    // no cache: drop it and refetch rather than run a broken
                    // tool.
                    warn!(
                        "ci_node: cached tool {} {} failed verification ({e}); refetching",
                        spec.name, tool.version
                    );
                    log(format!(
                        "[ci-node] tool {} {} — cached copy failed verification ({e}); refetching",
                        spec.name, tool.version
                    ));
                    let _ = std::fs::remove_dir_all(&dir);
                }
            }
        }

        let asset = (spec.asset)(&tool.version, host_triple()).ok_or_else(|| {
            format!(
                "tool '{}' publishes no prebuilt binary for {}",
                spec.name,
                host_triple()
            )
        })?;
        log(format!(
            "[ci-node] tool {} {} — fetching {}",
            spec.name, tool.version, asset.url
        ));
        install(root, spec, &tool.version, &asset, &dir).await?;
        log(format!(
            "[ci-node] tool {} {} — installed at {}",
            spec.name,
            tool.version,
            dir.display()
        ));
        dirs.push(dir);
    }
    Ok(dirs)
}

/// Download → extract → verify → atomically publish. Nothing is ever written
/// directly at `final_dir`; it appears only by renaming a staging directory
/// that has already passed [`verify`].
async fn install(
    root: &Path,
    spec: &ToolSpec,
    version: &str,
    asset: &Asset,
    final_dir: &Path,
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

    let bytes = download(&asset.url).await?;
    let staged_bin = tool_binary_path(staging.path(), spec);
    let wanted = staged_bin
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let kind = asset.kind;
    let dest = staged_bin.clone();
    let url = asset.url.clone();
    // Decompression is CPU-bound and blocking.
    tokio::task::spawn_blocking(move || extract_binary(&bytes, kind, &wanted, &dest, &url))
        .await
        .map_err(|e| format!("extract task panicked: {e}"))??;

    verify(&staged_bin, spec, version).await?;

    if let Some(parent) = final_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    // `into_path` disarms the auto-delete — from here the directory is either
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
            let bin = tool_binary_path(final_dir, spec);
            if bin.is_file() && verify(&bin, spec, version).await.is_ok() {
                info!(
                    "ci_node: tool {} {version} was published concurrently — adopting {}",
                    spec.name,
                    final_dir.display()
                );
                Ok(())
            } else {
                Err(format!(
                    "publish {} -> {}: {e}",
                    staged_path.display(),
                    final_dir.display()
                ))
            }
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
fn extract_binary(
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

/// Run the binary and require it to report the declared version. This is the
/// integrity check: the projects in the registry publish no checksum assets,
/// and executing the artifact catches strictly more than a checksum would
/// anyway — truncation, a wrong-platform asset, and a wrong-VERSION asset all
/// fail it, and the last is invisible to any hash we could have computed
/// ourselves.
async fn verify(bin: &Path, spec: &ToolSpec, version: &str) -> Result<(), String> {
    let mut cmd = crate::process_helpers::tokio_no_window(bin.to_string_lossy().as_ref());
    cmd.args(spec.verify_args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let out = match tokio::time::timeout(VERIFY_TIMEOUT, cmd.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(format!("spawn {}: {e}", bin.display())),
        Err(_) => {
            return Err(format!(
                "{} did not answer {:?} within {}s",
                bin.display(),
                spec.verify_args,
                VERIFY_TIMEOUT.as_secs()
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
            "{} {:?} exited {}",
            bin.display(),
            spec.verify_args,
            out.status
        ));
    }
    if !version_is_reported(&text, version) {
        return Err(format!(
            "{} reports a different version than the declared {version}",
            bin.display()
        ));
    }
    Ok(())
}

/// Does the tool's version output actually name `version`? Matched on a
/// whole token so `0.9.9` cannot be satisfied by `0.9.98`. Pure, so the
/// wrong-version case is testable without a download.
pub(crate) fn version_is_reported(output: &str, version: &str) -> bool {
    output
        .split(|c: char| {
            !(c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '+' || c == '_')
        })
        .any(|tok| tok == version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_closed() {
        assert!(lookup("cargo-nextest").is_some());
        assert!(lookup("cargo-install-anything").is_none());
        assert!(lookup("").is_none());
        assert_eq!(known_tool_names(), vec!["cargo-nextest"]);
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

    /// An unsupported platform is a legible refusal, not a mystery 404.
    #[test]
    fn unsupported_platform_resolves_to_no_asset() {
        assert!(cargo_nextest_asset("0.9.98", "riscv64gc-unknown-linux-gnu").is_none());
        assert!(cargo_nextest_asset("0.9.98", "unsupported").is_none());
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
    }
}
