fn main() {
    // Self-provision a `../dist/index.html` placeholder on a fresh worktree so a
    // bare `cargo check`/`cargo build` doesn't panic inside
    // `tauri::generate_context!` — `tauri.conf.json` pins
    // `frontendDist: "../dist"`, and a hand-created `git worktree` has no `dist/`
    // until `pnpm run build` runs. The supervisor's spawn path always builds the
    // frontend first, so this guard only ever fires for local-dev ergonomics.
    //
    // Touch-if-absent ONLY: we never overwrite a real built `dist/index.html`
    // (a successful `pnpm run build` always produces one before cargo runs, and
    // the `cargo:rerun-if-changed=../dist` below re-embeds it). The placeholder
    // is explicitly marked dev-only so nobody mistakes it for a real bundle if it
    // ever ships by accident.
    ensure_dist_placeholder();

    // Self-provision zero-byte `binaries/<sidecar>-<triple>` stubs (for every
    // `bundle.externalBin` sidecar: `qontinui_profile` + `qontinui-pr`) so
    // `tauri_build::build()` — which validates `bundle.externalBin` existence
    // during THIS build script on every `cargo build`/`check`/`test` — does not
    // fail before the real sidecars have been produced. The real binaries are
    // built by `npm run bundle:profile-sidecar` (wired into `beforeBuildCommand`)
    // at `tauri build` time and overwrite these stubs before bundling.
    ensure_sidecar_placeholders();

    // Tell Cargo to re-run this build script (and re-embed the frontend) when
    // the dist directory changes.  Without this, incremental builds silently
    // serve a stale frontend bundle from the compile-time cache.
    println!("cargo:rerun-if-changed=../dist");
    // Re-run when a Tauri capability changes so incremental builds re-embed the
    // updated ACL (e.g. adding a window label); otherwise gen/ stays stale and
    // the new permission silently never takes effect until a clean build.
    println!("cargo:rerun-if-changed=capabilities");
    // Force a re-run on every cargo build so RUNNER_BUILD_ID's millisecond
    // suffix actually changes between invocations. Without this, cargo will
    // cache the build-script output and re-stamp identical values into the
    // binary, defeating the SW-cache-invalidation watcher on rebuilds where
    // no other inputs changed.
    println!("cargo:rerun-if-changed=build.rs");

    // Embed the current git SHA so the running binary can report exactly which
    // commit it was built from. Surfaced via the runner /health endpoint and
    // the supervisor's spawn-test response — lets manual-test sessions assert
    // "this temp runner is the commit I'm debugging" without guessing from
    // binary mtime.
    let git_sha_short = std::process::Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=QONTINUI_GIT_SHA={}", git_sha_short);

    // RUNNER_BUILD_ID — the SW-cache-invalidation signal. Vite is the
    // single source of truth: `vite.config.ts` computes the value once
    // (`<git-sha-short>-<unix-ms>`), bakes it into index.html as
    // `<meta name="build-id">`, AND writes it to `dist/build-id.txt`.
    // We read that file here and re-emit the same string as the cargo env
    // so the meta tag and the binary's compile-time stamp match exactly
    // for any freshly-built binary. Without this, Vite's `Date.now()` and
    // cargo's `SystemTime::now()` capture would always differ by tens of
    // seconds, causing `useBuildIdWatcher` to fire on every cold spawn
    // instead of only on real mid-session binary swaps.
    //
    // Fallback: if `dist/build-id.txt` is missing (e.g. a manual `cargo
    // build` without a prior `npm run build`), compute fresh from
    // git+timestamp. This degrades gracefully — the binary is still
    // stamped with *something* — but the meta-tag/env match guarantee
    // only holds when the supervisor's standard build sequence runs
    // (`npm run build` → `cargo build`).
    let runner_build_id = std::fs::read_to_string("../dist/build-id.txt")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let git_sha = std::process::Command::new("git")
                .args(["rev-parse", "--short", "HEAD"])
                .output()
                .ok()
                .and_then(|o| {
                    if o.status.success() {
                        String::from_utf8(o.stdout)
                            .ok()
                            .map(|s| s.trim().to_string())
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| "unknown".to_string());
            let unix_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            format!("{}-{}", git_sha, unix_ms)
        });
    println!("cargo:rustc-env=RUNNER_BUILD_ID={}", runner_build_id);
    // Re-stamp when Vite writes a new build-id (covered by the broader
    // `../dist` rerun rule above, but keep the explicit hint so a future
    // refactor that narrows the dist watch doesn't silently freeze the id).
    println!("cargo:rerun-if-changed=../dist/build-id.txt");

    // Re-run this script when HEAD moves.
    //   - <git-dir>/HEAD fires on branch switch / detached-head jumps.
    //   - <common-dir>/refs/heads/ (directory) fires on any new commit to any
    //     local branch, since refs/heads/<branch> is the file git updates when
    //     advancing a branch ref. Without this a fresh commit on the
    //     currently-checked-out branch would keep the old QONTINUI_GIT_SHA
    //     embedded, defeating the purpose of this stamp.
    //
    // The paths MUST be resolved worktree-aware: in a linked `git worktree`,
    // `../.git` is a FILE (`gitdir: <path>`), so the former hardcoded
    // `../.git/HEAD` / `../.git/refs/heads` watches pointed at nonexistent
    // paths — and cargo re-runs the build script (recompiling this whole
    // crate) on EVERY invocation when a watched path does not exist. That made
    // each check/clippy/test in an agent worktree a full rebuild.
    let git_entry = std::path::Path::new("../.git");
    let git_dir = if git_entry.is_dir() {
        Some(git_entry.to_path_buf())
    } else {
        // Linked worktree: `.git` is a file `gitdir: <per-worktree git dir>`.
        std::fs::read_to_string(git_entry).ok().and_then(|s| {
            s.strip_prefix("gitdir:").map(|p| {
                let p = std::path::PathBuf::from(p.trim());
                if p.is_absolute() {
                    p
                } else {
                    std::path::Path::new("..").join(p)
                }
            })
        })
    };
    if let Some(git_dir) = git_dir {
        println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
        // refs/heads lives in the COMMON git dir; a linked worktree's git dir
        // has a `commondir` file pointing there (usually `../..`).
        let common_dir = std::fs::read_to_string(git_dir.join("commondir"))
            .map(|s| {
                let p = std::path::PathBuf::from(s.trim());
                if p.is_absolute() {
                    p
                } else {
                    git_dir.join(p)
                }
            })
            .unwrap_or_else(|_| git_dir.clone());
        println!(
            "cargo:rerun-if-changed={}",
            common_dir.join("refs/heads").display()
        );
    }
    // No git dir at all (source tarball): emit no watch — a stable fingerprint
    // beats a nonexistent-path watch that forces a rebuild every run.

    // Generate the Rust `VALID_TAB_IDS` gate FROM the TypeScript `MainTabId`
    // union — one source of truth, not two hand-maintained lists (iter-2 R2).
    generate_valid_tab_ids();

    // Section 4 (UI Bridge redesign) — refresh the runner's IR baseline if
    // any tsx source is newer. Best-effort only; never fails the build.
    refresh_ir_if_stale();

    tauri_build::build()
}

/// Emit `$OUT_DIR/valid_tab_ids.rs` containing the `VALID_TAB_IDS` slice used
/// by `mcp::ui_bridge::page` to gate `set-tab` / `tab/activate`.
///
/// WHY (manual-test-loop iter 2, R2): the Rust slice used to be a hand-copied
/// mirror of `VALID_TAB_IDS` in `src/components/app/tab-types.ts`, with a
/// comment that said "Kept in sync manually". It wasn't — the two lists had
/// drifted (103 Rust entries vs 106 TS), so ids the frontend advertised via
/// `GET /control/tabs` were rejected as `unknown_tab` by `tab/activate`. Two
/// hand-maintained copies of one truth always drift; the fix is to stop having
/// two. The TS union stays authoritative (it is what actually renders) and the
/// Rust gate is derived from it at build time.
///
/// This is deliberately FATAL on failure. An empty or missing list would make
/// the runner reject every tab id at runtime — far worse than a build error.
fn generate_valid_tab_ids() {
    use std::path::Path;

    const TAB_TYPES_TS: &str = "../src/components/app/tab-types.ts";
    println!("cargo:rerun-if-changed={TAB_TYPES_TS}");

    let source = std::fs::read_to_string(Path::new(TAB_TYPES_TS)).unwrap_or_else(|e| {
        panic!(
            "qontinui-runner build.rs: cannot read {TAB_TYPES_TS} (the source of truth for \
             VALID_TAB_IDS): {e}"
        )
    });

    let ids = parse_ts_valid_tab_ids(&source);
    assert!(
        ids.len() > 50,
        "qontinui-runner build.rs: parsed only {} tab id(s) from {TAB_TYPES_TS} — the \
         `const VALID_TAB_IDS: MainTabId[] = [...]` literal must have changed shape. Fix the \
         parser rather than shipping a gate that rejects valid tabs.",
        ids.len()
    );
    {
        let mut seen = std::collections::BTreeSet::new();
        for id in &ids {
            assert!(
                seen.insert(id.clone()),
                "qontinui-runner build.rs: duplicate tab id {id:?} in {TAB_TYPES_TS}"
            );
        }
    }

    let mut out = String::from(
        "// @generated by src-tauri/build.rs from src/components/app/tab-types.ts — DO NOT EDIT.\n\
         // Add or remove tab ids in the TypeScript `MainTabId` union; this mirror follows.\n\
         const VALID_TAB_IDS: &[&str] = &[\n",
    );
    for id in &ids {
        out.push_str(&format!("    {id:?},\n"));
    }
    out.push_str("];\n");

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR is always set for build scripts");
    let dest = Path::new(&out_dir).join("valid_tab_ids.rs");
    std::fs::write(&dest, out).unwrap_or_else(|e| {
        panic!(
            "qontinui-runner build.rs: failed to write {}: {e}",
            dest.display()
        )
    });
}

/// Extract the string literals from the `const VALID_TAB_IDS: MainTabId[] = [ … ];`
/// array in `tab-types.ts`.
///
/// A deliberate 20-line scanner rather than a TS parser dep: the literal is a
/// flat list of double-quoted strings, and `generate_valid_tab_ids` asserts a
/// sane count, so a shape change fails the build loudly instead of silently
/// producing a short list.
fn parse_ts_valid_tab_ids(source: &str) -> Vec<String> {
    let Some(decl) = source.find("const VALID_TAB_IDS") else {
        panic!(
            "qontinui-runner build.rs: `const VALID_TAB_IDS` not found in tab-types.ts — the \
             Rust tab-id gate is generated from it"
        );
    };
    let rest = &source[decl..];
    let open = rest
        .find("= [")
        .expect("VALID_TAB_IDS declaration must be an array literal (`= [`)")
        + "= [".len();
    let close = rest[open..]
        .find("];")
        .expect("VALID_TAB_IDS array literal must be terminated by `];`")
        + open;

    let mut ids = Vec::new();
    let mut chars = rest[open..close].chars();
    let mut current: Option<String> = None;
    while let Some(c) = chars.next() {
        match (&mut current, c) {
            (None, '"') => current = Some(String::new()),
            (Some(buf), '\\') => {
                if let Some(escaped) = chars.next() {
                    buf.push(escaped);
                }
            }
            (Some(_), '"') => {
                ids.push(current.take().expect("in-progress literal"));
            }
            (Some(buf), other) => buf.push(other),
            (None, _) => {}
        }
    }
    ids
}

/// Create a minimal `../dist/index.html` placeholder **only if it is absent**,
/// so a fresh `git worktree` (which has no built `dist/`) can `cargo check`
/// without a manual pre-step. Never overwrites an existing file — a real
/// `pnpm run build` always wins because it runs before cargo and produces the
/// genuine bundle, and the `cargo:rerun-if-changed=../dist` rule re-embeds it.
///
/// Best-effort: any IO error is downgraded to a `cargo:warning=` so a
/// permission/race hiccup degrades to the prior behavior (a `generate_context!`
/// panic with a clear cause) rather than failing the build script outright.
fn ensure_dist_placeholder() {
    use std::path::Path;

    let dist_index = Path::new("../dist/index.html");
    if dist_index.exists() {
        // Real (or previously-placeheld) bundle present — leave it untouched.
        return;
    }

    // Honest, self-documenting stub: explicitly marks itself a dev-only build
    // artifact so it can't be mistaken for the real frontend if it somehow ends
    // up shipped. A genuine production build overwrites this before embedding.
    const PLACEHOLDER: &str = "<!doctype html>\n<title>qontinui-runner dev placeholder</title>\n<!-- Auto-generated by src-tauri/build.rs on a worktree with no built dist/.\n     This is a DEV-ONLY stub so `cargo check` can run before `pnpm run build`.\n     A real build (`pnpm run build`) overwrites it with the actual bundle. -->\n<p>qontinui-runner frontend not built yet — run `pnpm run build`.</p>\n";

    if let Err(e) = std::fs::create_dir_all("../dist") {
        println!(
            "cargo:warning=qontinui-runner: failed to create ../dist for the dev placeholder: {e}"
        );
        return;
    }
    if let Err(e) = std::fs::write(dist_index, PLACEHOLDER) {
        println!(
            "cargo:warning=qontinui-runner: failed to write the ../dist/index.html dev placeholder: {e}"
        );
    }
}

/// Self-provision zero-byte `binaries/<sidecar>-<target-triple>` placeholders
/// if absent — one per `bundle.externalBin` entry (`qontinui_profile`,
/// `qontinui-pr`) — so `tauri_build::build()` (which validates
/// `bundle.externalBin` existence during this build script — hence on every
/// `cargo build`/`check`/`test`, not just `tauri build`) does not fail when the
/// real sidecars have not been produced yet.
///
/// The REAL binaries are produced by `npm run bundle:profile-sidecar` (wired
/// into `beforeBuildCommand`) at `tauri build` time, which overwrites these
/// placeholders before the bundle is assembled. So a zero-byte stub only ever
/// exists on cargo-only paths (CI `cargo test`, the supervisor's `cargo build`,
/// local `cargo check`) where nothing is bundled — never in a shipped installer
/// (the sidecar script fails loud if it can't build a real binary). Mirrors
/// `ensure_dist_placeholder()`.
fn ensure_sidecar_placeholders() {
    use std::path::Path;

    // Cargo sets TARGET for build scripts to the triple being compiled; the
    // externalBin path is `binaries/<name>-<triple>[.exe]` relative to
    // src-tauri (this build script's CWD).
    let Ok(target) = std::env::var("TARGET") else {
        return;
    };
    let ext = if target.contains("windows") {
        ".exe"
    } else {
        ""
    };
    // Keep in sync with `tauri.conf.json` `bundle.externalBin` and
    // `scripts/bundle-profile-sidecar.mjs` SIDECAR_BINS.
    for name in ["qontinui_profile", "qontinui-pr"] {
        let rel = format!("binaries/{name}-{target}{ext}");
        let path = Path::new(&rel);
        if path.exists() {
            // A real binary (from bundle:profile-sidecar) or a prior
            // placeholder is already present — never clobber a real one.
            continue;
        }
        if let Err(e) = std::fs::create_dir_all("binaries") {
            println!(
                "cargo:warning=qontinui-runner: failed to create binaries/ for the {name} sidecar placeholder: {e}"
            );
            return;
        }
        if let Err(e) = std::fs::write(path, b"") {
            println!(
                "cargo:warning=qontinui-runner: failed to write the {name} sidecar placeholder: {e}"
            );
        }
    }
}

/// Re-emit the runner's IR documents if any tsx source under ../src is newer
/// than the IR baseline at ../specs/pages/runner-root/state-machine.derived.json.
///
/// Piggybacks on the existing `cargo:rerun-if-changed=../dist` rule (which
/// already re-fires when the frontend rebuilds), so we don't add a parallel
/// `rerun-if-changed=../src` watch — that would over-fire on hot-reload-only
/// edits during dev. The dist watch is a sufficient proxy: a fresh frontend
/// bundle always implies fresh tsx sources, so the staleness check below
/// catches the meaningful changes without coupling the build script to every
/// keystroke.
///
/// Failure modes are non-fatal — if the IR build fails, we print a warning
/// (visible via `cargo:warning=`) and let the runner build proceed. The
/// embedded `include_dir!` snapshot of `../specs/pages` is the safety net for
/// production binaries: even if a fresh emit didn't happen, the previous IR
/// is baked in.
///
/// The IR build is gated to avoid running by default on every cargo build:
///   - If `QONTINUI_BUILD_IR=1` is set, always attempt.
///   - Otherwise, attempt only when a package manager (pnpm preferred when
///     `pnpm-lock.yaml` exists, else npm) is on PATH and the runner's
///     `package.json` has a `build-ir` script.
///   - Otherwise skip silently with a `cargo:warning=` hint.
fn refresh_ir_if_stale() {
    use std::path::Path;

    // Page id matches package.json's `build-ir` script (`--document-id=runner-root`).
    let baseline = Path::new("../specs/pages/runner-root/state-machine.derived.json");
    // Baseline missing — first-run case; skip silently.
    let baseline_mtime = std::fs::metadata(baseline).and_then(|m| m.modified()).ok();

    // Walk ../src for *.tsx files; collect newest mtime. We do this manually
    // to avoid pulling in a walkdir build-dep. Recursive depth is unbounded
    // but the runner's src tree is small enough that this is fine.
    let src_dir = Path::new("../src");
    let newest_tsx_mtime = newest_tsx_mtime(src_dir);

    // If both timestamps available, compare; otherwise skip (no signal).
    let needs_refresh = match (baseline_mtime, newest_tsx_mtime) {
        (Some(baseline_t), Some(src_t)) => src_t > baseline_t,
        // No baseline yet — the embedded snapshot will fall back to whatever
        // is currently on disk (possibly nothing for runner-root). Attempt
        // the emit so future builds have a baseline.
        (None, Some(_)) => true,
        _ => false,
    };

    if !needs_refresh {
        return;
    }

    // Gate: explicit env var, OR package-manager-on-PATH + package.json has build-ir script.
    let force = std::env::var("QONTINUI_BUILD_IR").ok().as_deref() == Some("1");
    let pm = detect_package_manager();
    let auto_eligible = pm.is_some() && package_json_has_build_ir();
    if !force && !auto_eligible {
        println!(
            "cargo:warning=qontinui-runner IR is stale but build-ir was skipped (set QONTINUI_BUILD_IR=1 or ensure npm/pnpm is on PATH with a `build-ir` script in package.json)"
        );
        return;
    }

    let runner_root = Path::new("..");
    let pm_cmd = pm.unwrap_or("npm");

    // On Windows, npm/pnpm are .cmd shims that bare std::process::Command
    // can't resolve — wrap via `cmd /C` for portability.
    #[cfg(windows)]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", pm_cmd, "run", "build-ir"]);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = std::process::Command::new(pm_cmd);
        c.args(["run", "build-ir"]);
        c
    };
    cmd.current_dir(runner_root);

    match cmd.output() {
        Ok(output) if output.status.success() => {
            // Quiet success — IR is fresh.
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            // cargo:warning= can only emit a single line — collapse whitespace.
            let combined = format!("{} {}", stderr.trim(), stdout.trim());
            let single_line: String = combined
                .chars()
                .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
                .collect();
            println!(
                "cargo:warning=qontinui-runner build-ir failed (exit {}): {}",
                output.status, single_line
            );
        }
        Err(e) => {
            println!(
                "cargo:warning=qontinui-runner build-ir invocation failed: {}",
                e
            );
        }
    }
}

/// Walk `dir` recursively and return the newest mtime among `*.tsx` files.
fn newest_tsx_mtime(dir: &std::path::Path) -> Option<std::time::SystemTime> {
    if !dir.exists() {
        return None;
    }
    let mut newest: Option<std::time::SystemTime> = None;
    let mut stack: Vec<std::path::PathBuf> = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(it) => it,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let ft = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if ft.is_dir() {
                // Skip node_modules and similar — they explode the walk and
                // never contain authoring-time tsx anyway.
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if matches!(name, "node_modules" | ".next" | "dist" | "build") {
                        continue;
                    }
                }
                stack.push(path);
            } else if ft.is_file() && path.extension().and_then(|e| e.to_str()) == Some("tsx") {
                if let Ok(meta) = entry.metadata() {
                    if let Ok(modified) = meta.modified() {
                        newest = Some(newest.map_or(modified, |cur| cur.max(modified)));
                    }
                }
            }
        }
    }
    newest
}

/// Detect the project's package manager by lockfile, then verify it's on PATH.
/// Returns `Some("pnpm")` or `Some("npm")`, or `None` if neither is available.
fn detect_package_manager() -> Option<&'static str> {
    let has_pnpm_lock = std::path::Path::new("../pnpm-lock.yaml").exists();

    let probe = |name: &str| -> bool {
        #[cfg(windows)]
        let result = std::process::Command::new("cmd")
            .args(["/C", name, "--version"])
            .output();
        #[cfg(not(windows))]
        let result = std::process::Command::new(name).arg("--version").output();
        matches!(result, Ok(o) if o.status.success())
    };

    if has_pnpm_lock && probe("pnpm") {
        return Some("pnpm");
    }
    if probe("npm") {
        return Some("npm");
    }
    None
}

/// Check whether `../package.json` declares a `build-ir` script. We do a
/// substring scan rather than a full JSON parse to avoid a serde_json
/// build-dep. The match is intentionally conservative.
fn package_json_has_build_ir() -> bool {
    match std::fs::read_to_string("../package.json") {
        Ok(s) => s.contains("\"build-ir\""),
        Err(_) => false,
    }
}
