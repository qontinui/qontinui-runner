fn main() {
    // Fail FAST and legibly when `debug-tokio-console` is on without the
    // build-wide `--cfg tokio_unstable` rustc flag it requires. Without this
    // guard the failure is a runtime `assert!` deep inside
    // `ConsoleLayer::build` (console-subscriber ships no build script of its
    // own), i.e. a binary that compiles and then panics on startup.
    guard_tokio_console_cfg();

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
    // Force a re-run on every cargo build so RUNNER_BUILD_ID is re-read from
    // the current `dist/build-id.txt` on every invocation. Without this, cargo
    // caches the build-script output and re-stamps a stale value into the
    // binary, so /health would report provenance for a dist the exe no longer
    // embeds on rebuilds where no other input changed.
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

    // RUNNER_BUILD_ID — compile-time build provenance, surfaced on /health.
    // Vite is the single source of truth: `vite.config.ts` computes the value
    // once (`<git-sha-short>-<unix-ms>`), bakes it into index.html as
    // `<meta name="build-id">`, AND writes it to `dist/build-id.txt`.
    // We read that file here and re-emit the same string as the cargo env, so
    // the binary reports the identity of the dist it actually embedded rather
    // than an unrelated value invented at cargo time.
    //
    // Fallback: if `dist/build-id.txt` is missing (e.g. a bare `cargo build` /
    // `cargo check` with no prior `pnpm run build`), emit the explicit
    // `unstamped-<git-sha>` sentinel plus a cargo warning. The old behaviour
    // — inventing `git sha + SystemTime::now()` — was silently and
    // permanently WRONG: it baked a build-id that matched no dist anywhere,
    // and /health then reported that invention as if it were provenance
    // (plan 2026-07-28-runner-build-id-banner-permanent-false-positive, D1).
    // A sentinel says "this build did not come from a Vite dist" out loud.
    //
    // Deliberately a sentinel and NOT a hard failure: a bare cargo build with
    // no dist is the normal inner dev loop (`cargo check` / `cargo test` /
    // `cargo-guard.sh`), and panicking here would break it. The supervisor's
    // pre-cargo gate is where this fails hard — `verify_frontend_built` /
    // `dist_index_ok` in qontinui-supervisor refuse to hand off to cargo
    // without a non-empty `dist/build-id.txt`. The two cover disjoint entry
    // points: the gate covers supervisor-driven builds, this sentinel covers
    // manual ones the supervisor never sees.
    let git_sha_for_fallback = || {
        std::process::Command::new("git")
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
            .unwrap_or_else(|| "unknown".to_string())
    };
    let runner_build_id = std::fs::read_to_string("../dist/build-id.txt")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let sentinel = format!("unstamped-{}", git_sha_for_fallback());
            println!(
                "cargo:warning=dist/build-id.txt missing or empty — stamping \
                 RUNNER_BUILD_ID={}. This binary did not come from a Vite dist; \
                 /health will report it as unstamped. Run `pnpm run build` \
                 before `cargo build` for a real build-id.",
                sentinel
            );
            sentinel
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

    // Generate the Rust `VALID_NAVIGATE_PAGES` gate FROM the TypeScript
    // `PAGE_TO_TAB` map — the only thing that decides whether a
    // `page/navigate` target actually goes anywhere (iter-12 item 3).
    generate_valid_navigate_pages();

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

/// Emit `$OUT_DIR/valid_navigate_pages.rs` containing the
/// `VALID_NAVIGATE_PAGES` slice used by `mcp::ui_bridge::page` to gate
/// `POST /control/page/navigate`.
///
/// WHY (manual-test-loop iter 12, item 3): `page/navigate` accepted ANY
/// relative path and answered `success: true`. The runner has no URL router —
/// the frontend turns the path into a page key, looks it up in `PAGE_TO_TAB`,
/// and does nothing when the key is absent — but it had already run
/// `history.pushState(url)`, so the address bar (and therefore the snapshot's
/// `route`) echoed a page the app never navigated to. Every future
/// manual-test run that navigated somewhere unrouted would have read its own
/// echo back as proof it arrived: a false-PASS generator.
///
/// `PAGE_TO_TAB` is authoritative because it is the map the running app
/// actually consults; deriving the gate from it at build time is the same
/// no-second-copy discipline `generate_valid_tab_ids` established, and for the
/// same reason — the hand-copied version of that gate had already drifted.
///
/// Deliberately FATAL on failure: a missing or truncated list would make the
/// runner reject navigation to real pages, which is worse than a build error.
fn generate_valid_navigate_pages() {
    use std::path::Path;

    const NAV_TS: &str = "../src/components/app/useAppNavigation.ts";
    println!("cargo:rerun-if-changed={NAV_TS}");

    let source = std::fs::read_to_string(Path::new(NAV_TS)).unwrap_or_else(|e| {
        panic!(
            "qontinui-runner build.rs: cannot read {NAV_TS} (the source of truth for \
             PAGE_TO_TAB / VALID_NAVIGATE_PAGES): {e}"
        )
    });

    let pages = parse_ts_page_to_tab_keys(&source);
    assert!(
        pages.len() > 50,
        "qontinui-runner build.rs: parsed only {} navigable page key(s) from {NAV_TS} — the \
         `PAGE_TO_TAB` object literal must have changed shape. Fix the parser rather than \
         shipping a gate that rejects real pages.",
        pages.len()
    );
    {
        let mut seen = std::collections::BTreeSet::new();
        for page in &pages {
            assert!(
                seen.insert(page.clone()),
                "qontinui-runner build.rs: duplicate PAGE_TO_TAB key {page:?} in {NAV_TS}"
            );
        }
    }

    let mut out = String::from(
        "// @generated by src-tauri/build.rs from src/components/app/useAppNavigation.ts \
         — DO NOT EDIT.\n\
         // Add or remove navigable pages in the TypeScript `PAGE_TO_TAB` map; this \
         mirror follows.\n\
         const VALID_NAVIGATE_PAGES: &[&str] = &[\n",
    );
    for page in &pages {
        out.push_str(&format!("    {page:?},\n"));
    }
    out.push_str("];\n");

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR is always set for build scripts");
    let dest = Path::new(&out_dir).join("valid_navigate_pages.rs");
    std::fs::write(&dest, out).unwrap_or_else(|e| {
        panic!(
            "qontinui-runner build.rs: failed to write {}: {e}",
            dest.display()
        )
    });
}

/// Extract the KEYS of the `PAGE_TO_TAB: Record<string, MainTabId>` object
/// literal in `useAppNavigation.ts`.
///
/// The literal mixes quoted keys (`"prompt-home": "prompt-home",`) with bare
/// identifier keys (`home: "prompt-home",`) and carries `//` section comments,
/// so this is a line scanner rather than the character scanner
/// `parse_ts_valid_tab_ids` uses. Same tradeoff though: a deliberate small
/// parser plus a sanity assert on the count, so a shape change fails the build
/// loudly instead of silently producing a short list.
fn parse_ts_page_to_tab_keys(source: &str) -> Vec<String> {
    let Some(decl) = source.find("const PAGE_TO_TAB") else {
        panic!(
            "qontinui-runner build.rs: `const PAGE_TO_TAB` not found in useAppNavigation.ts — \
             the Rust navigate gate is generated from it"
        );
    };
    let rest = &source[decl..];
    let open = rest
        .find("= {")
        .expect("PAGE_TO_TAB declaration must be an object literal (`= {`)")
        + "= {".len();
    let close = rest[open..]
        .find("\n};")
        .expect("PAGE_TO_TAB object literal must be terminated by a line-initial `};`")
        + open;

    let mut keys = Vec::new();
    for raw_line in rest[open..close].lines() {
        // Strip trailing `//` comments, then whitespace. No key contains
        // `//`, so a plain split is sound here.
        let line = raw_line.split("//").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some(colon) = line.find(':') else {
            continue;
        };
        let key = line[..colon].trim();
        let key = match key.strip_prefix('"') {
            Some(inner) => inner.strip_suffix('"').unwrap_or_else(|| {
                panic!("qontinui-runner build.rs: unterminated PAGE_TO_TAB key in {line:?}")
            }),
            None => key,
        };
        if key.is_empty() {
            continue;
        }
        keys.push(key.to_string());
    }
    keys
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

/// Refuse to build `--features debug-tokio-console` unless the build also
/// carries `--cfg tokio_unstable`.
///
/// `console-subscriber` only functions when the whole dependency graph —
/// tokio included — was compiled with `--cfg tokio_unstable`; without it
/// `ConsoleLayer::build` trips its own `assert!` at *runtime*, so the mistake
/// costs a full build plus a launch before it is visible. That flag is
/// build-wide (a rustc `--cfg`, not a Cargo feature), so Cargo cannot set it
/// for one feature only.
///
/// It is deliberately set NOWHERE in this repository — not in
/// `.cargo/config.toml`, not here — because a `[build] rustflags` entry would
/// apply unconditionally to *every* build of this crate, including the shipped
/// release bundle. The developer passes it at invocation time instead
/// (`scripts/dev-tokio-console.sh` / `.ps1` do it for you), and this guard
/// turns the "forgot it" case into a one-line error.
///
/// Reads `CARGO_ENCODED_RUSTFLAGS` (the authoritative, `\x1f`-separated list
/// Cargo hands every build script, which already folds in `.cargo/config.toml`)
/// and falls back to the raw `RUSTFLAGS` string.
fn guard_tokio_console_cfg() {
    // Cargo only re-runs this script when a watched input changes; without
    // these the guard would go stale after a RUSTFLAGS change.
    println!("cargo:rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");
    println!("cargo:rerun-if-env-changed=RUSTFLAGS");

    if std::env::var_os("CARGO_FEATURE_DEBUG_TOKIO_CONSOLE").is_none() {
        // Feature off — the normal build. Nothing to check, and nothing in
        // this function ever sets a flag.
        return;
    }

    let encoded = std::env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_default();
    let has_cfg = encoded
        .split('\x1f')
        .any(|flag| flag.trim() == "tokio_unstable")
        || std::env::var("RUSTFLAGS")
            .unwrap_or_default()
            .split_whitespace()
            .any(|flag| flag == "tokio_unstable");

    if !has_cfg {
        panic!(
            "\n\n\
             qontinui-runner: feature `debug-tokio-console` requires \
             RUSTFLAGS=\"--cfg tokio_unstable\".\n\n\
             Run one of these instead:\n\
             \x20   scripts/dev-tokio-console.sh   run           # bash / WSL\n\
             \x20   scripts/dev-tokio-console.ps1  -Action run   # PowerShell\n\n\
             Or set it by hand:\n\
             \x20   RUSTFLAGS=\"--cfg tokio_unstable\" cargo run --features debug-tokio-console\n\n\
             This flag is intentionally NOT set in .cargo/config.toml: it is \
             build-wide, so pinning it there would put tokio's unstable API \
             surface into the shipped release build too. Note that changing \
             RUSTFLAGS invalidates the build cache — expect a full rebuild of \
             the dependency graph. See src-tauri/docs/tokio-console.md.\n"
        );
    }
}
