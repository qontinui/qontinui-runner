// `build.rs` is ALSO compiled into the library's test binary, via a
// `#[cfg(test)] #[path = "../build.rs"] mod build_script;` declaration in
// `src/wedge_diagnostics.rs`. That is the only way `cargo test` can execute the
// unit tests below: Cargo compiles a build script as its own crate and never in
// test mode, so a `#[cfg(test)] mod tests` here would otherwise be dead text
// that no CI run ever proves. `main` is gated off that compilation because it is
// the one item that reaches for `tauri_build`, a build-dependency the library
// cannot resolve.
#[cfg(not(test))]
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
    // Re-run when this script itself changes. This does NOT "force a re-run on
    // every cargo build", which is what this comment used to claim: emitting
    // ANY `rerun-if-changed` NARROWS cargo's default ("any file in the
    // package") to exactly the paths listed, so this line watches build.rs and
    // nothing else. `dist/build-id.txt` is re-read because `../dist` is watched
    // above — not because of this line (plan
    // 2026-08-23-build-provenance-assertion, D1).
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

    // Stamp tree-state provenance (git dirty bit, tree hash, Rust-source hash,
    // the embedded dist's frontend-source hash) and warn when the embedded dist
    // no longer matches the frontend sources it was built from. Never fatal.
    stamp_provenance();

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
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
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
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
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
/// The parse itself lives in [`rustflags_carry_tokio_unstable`], which honours
/// Cargo's precedence between `CARGO_ENCODED_RUSTFLAGS` and `RUSTFLAGS` and
/// accepts both `--cfg tokio_unstable` and `--cfg=tokio_unstable`.
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

    let encoded = std::env::var("CARGO_ENCODED_RUSTFLAGS").ok();
    let raw = std::env::var("RUSTFLAGS").ok();
    if rustflags_carry_tokio_unstable(encoded.as_deref(), raw.as_deref()) {
        return;
    }

    panic!(
        "\n\n\
         qontinui-runner: feature `debug-tokio-console` requires \
         RUSTFLAGS=\"--cfg tokio_unstable\".\n\n\
         Run one of these instead:\n\
         \x20   scripts/dev-tokio-console.sh   run           # bash / WSL\n\
         \x20   scripts/dev-tokio-console.ps1  -Action run   # PowerShell\n\n\
         Or set it by hand (either spelling is accepted):\n\
         \x20   RUSTFLAGS=\"--cfg tokio_unstable\" cargo run --features debug-tokio-console\n\
         \x20   RUSTFLAGS=\"--cfg=tokio_unstable\" cargo run --features debug-tokio-console\n\n\
         This flag is intentionally NOT set in .cargo/config.toml: it is \
         build-wide, so pinning it there would put tokio's unstable API \
         surface into the shipped release build too. Note that changing \
         RUSTFLAGS invalidates the build cache — expect a full rebuild of \
         the dependency graph. See src-tauri/docs/tokio-console.md.\n"
    );
}

/// Does the build actually carry `--cfg tokio_unstable`?
///
/// Pure — a function of the two environment strings alone — so the parsing this
/// guard's whole value rests on is unit-tested instead of only ever exercised by
/// a developer hitting the panic.
///
/// **Cargo's precedence, honoured rather than `||`-ed.** `CARGO_ENCODED_RUSTFLAGS`
/// is the authoritative, `\x1f`-separated list Cargo will hand rustc; it already
/// folds in `RUSTFLAGS`, `.cargo/config.toml`'s `[build] rustflags`, and
/// per-target `rustflags`. So when it is **present** it is the whole truth and
/// `RUSTFLAGS` must be ignored — including when it is present and *empty*, which
/// is exactly the "a stale `RUSTFLAGS` is exported in this shell but Cargo is not
/// forwarding it" case. `||`-ing the two lets that stale value satisfy the guard
/// while rustc never sees the cfg, which reproduces the runtime
/// `ConsoleLayer::build` assert this guard exists to prevent. The raw `RUSTFLAGS`
/// arm is therefore a fallback for the only case it can be right about: a
/// non-Cargo invocation where the encoded variable is absent entirely.
fn rustflags_carry_tokio_unstable(encoded: Option<&str>, raw: Option<&str>) -> bool {
    match encoded {
        // `\x1f`-separated, and Cargo does NOT re-split on whitespace: a field
        // is one flag.
        Some(e) => flags_carry_tokio_unstable(e.split('\x1f')),
        // Absent means "not invoked by a Cargo that sets it". `RUSTFLAGS` is a
        // single string rustc-style, so it whitespace-splits.
        None => raw.is_some_and(|r| flags_carry_tokio_unstable(r.split_whitespace())),
    }
}

/// Scan an already-split flag list for `--cfg tokio_unstable`.
///
/// **Both spellings, because rustc accepts both and developers write both.**
/// `--cfg tokio_unstable` arrives as two tokens; `--cfg=tokio_unstable` arrives
/// as one. The previous guard compared every token against the bare string
/// `"tokio_unstable"`, so the joined spelling — the one
/// `src-tauri/.cargo/config.toml` already uses for `--remap-path-prefix=…`, and
/// therefore the one a developer here would plausibly copy — failed the guard
/// and told them to set a flag they had already set.
///
/// A bare `tokio_unstable` token that is NOT preceded by `--cfg` is deliberately
/// not a match: it is not a cfg, and accepting it would let an unrelated flag
/// value wave the guard through.
fn flags_carry_tokio_unstable<'a, I: IntoIterator<Item = &'a str>>(flags: I) -> bool {
    let mut prev_was_cfg = false;
    for flag in flags {
        let flag = flag.trim();
        if flag.is_empty() {
            continue;
        }
        if prev_was_cfg && flag == "tokio_unstable" {
            return true;
        }
        if let Some(value) = flag.strip_prefix("--cfg=") {
            if value.trim() == "tokio_unstable" {
                return true;
            }
        }
        prev_was_cfg = flag == "--cfg";
    }
    false
}

// ---------------------------------------------------------------------------
// Build provenance (plan 2026-08-23-build-provenance-assertion, Phases 1b + 2)
// ---------------------------------------------------------------------------
//
// The runner is built in two independent steps — Vite writes `dist/`, cargo
// embeds it — and nothing asserted they describe the same tree. These stamps
// record WHAT TREE STATE the binary was built from as content hashes, because
// agents build from dirty worktrees as normal practice and a commit SHA says
// nothing about uncommitted edits. `/health` surfaces them under `provenance`.
//
// This is a Rust PORT of `fold_lines` in `scripts/frontend-provenance.mjs`,
// which is the canonical implementation (vite and the temp-runner launcher use
// it). Both are pinned to `scripts/fixtures/provenance-fold.json` by a test on
// each side — change the format in both or neither. The format:
//   * one line per input, `<repo-relative path> <40-hex oid>`, or
//     `<path> absent` for an input recorded at build time and since deleted;
//   * git's own path spelling verbatim, sorted by UTF-8 byte value;
//   * every line `\n`-terminated, the last included.
// Oids and the fold are hashed with `git hash-object` on both sides, so
// `core.autocrlf` normalization is identical everywhere.
//
// What these stamps can and cannot see (D1): cargo re-runs this script only
// when a WATCHED path moves, so every stamp here is "as of the LAST
// build-script run", not "as of this compile". Neither `src` nor `../src` is
// watched, on purpose: a re-run changes these env values, and changed
// build-script output rebuilds EVERY target of the package -- a bin-only edit
// would recompile the whole lib and all bins, and a `.tsx` edit the whole
// runner, under rust-analyzer too. Instead, `pnpm run build:exe`
// (`scripts/build-exe.mjs`) sets QONTINUI_PROVENANCE_NONCE to a fresh value,
// and this script declares `rerun-if-env-changed` on it: every build:exe
// re-stamps, whether or not its Vite step rewrote `../dist`. Any build that
// rewrites `../dist` (a watched path; `build-id.txt` carries a timestamp) also
// re-stamps. What may NOT re-stamp is a bare `cargo build` after a Rust edit,
// or the supervisor's path that builds on a PRIOR `dist/` after a failed
// `npm run build` (its `frontend_stale_any`): that exe carries the previous
// run's stamps. A launch-time `scripts/frontend-provenance.mjs verify` then
// usually reports the Rust half as a mismatch naming `build:exe` -- but NOT
// always: if the worktree is later returned to the stamped state (a stash, a
// checkout, a revert) the comparison matches an exe built from different code.
// The supervisor setting the nonce too closes its half of that; it is a
// follow-up in qontinui-supervisor.
//
// Recompute failure is `unknown`, never `false` (D3): no git, no repo, an
// unreadable input — the check produced no verdict, and saying "disagree"
// would be a lie about a correct tree.

/// Repo-root-relative pathspecs of the Rust half's binary-affecting inputs:
/// the crate, its in-repo path dependencies, the vendored `[patch]`, and what
/// `generate_context!` / `include_str!` embed. Must equal `RUST_SRC_PATHSPECS`
/// in `scripts/frontend-provenance.mjs`. The `../qontinui-schemas` sibling is
/// out of scope on purpose (sibling-pin-check.sh owns sibling drift).
const RUST_SRC_PATHSPECS: &[&str] = &[
    "src-tauri/src",
    "src-tauri/build.rs",
    "src-tauri/Cargo.toml",
    "src-tauri/capabilities",
    "src-tauri/tauri.conf.json",
    "src-tauri/resources",
    "src-tauri/icons",
    "src-tauri/clorinde",
    "crates/spec-check",
    "crates/runner-stats",
    "crates/runner-win32",
    "vendor/tao-0.35.0",
    "Cargo.toml",
    "Cargo.lock",
];

const PROVENANCE_UNKNOWN: &str = "unknown";

/// The fold text for `(path, oid)` entries; `None` is an absent input.
fn fold_lines(entries: &[(String, Option<String>)]) -> String {
    let mut rows: Vec<(String, &Option<String>)> = entries
        .iter()
        .map(|(p, o)| (p.replace('\\', "/"), o))
        .collect();
    // `str`'s `Ord` is byte-lexicographic over UTF-8 — LC_ALL=C order.
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = String::new();
    for (path, oid) in rows {
        out.push_str(&path);
        out.push(' ');
        out.push_str(oid.as_deref().unwrap_or("absent"));
        out.push('\n');
    }
    out
}

fn git_output(
    root: &std::path::Path,
    args: &[&str],
    stdin: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("git")
        // A git inherited from a hook would answer about a different repository
        // or index than `-C root` names.
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if let Some(bytes) = stdin {
        // Feed stdin from a thread so a large stdout cannot deadlock the pipe.
        let mut pipe = child.stdin.take().ok_or("git stdin unavailable")?;
        let bytes = bytes.to_vec();
        let writer = std::thread::spawn(move || pipe.write_all(&bytes));
        let out = child
            .wait_with_output()
            .map_err(|e| format!("git {args:?} failed: {e}"))?;
        let wrote = writer.join();
        // Git's own exit status first: a git that failed early surfaces to the
        // writer as a broken pipe, and its stderr is the useful half.
        let stdout = finish_git(args, out)?;
        wrote
            .map_err(|_| "git stdin writer panicked".to_string())?
            .map_err(|e| format!("git {args:?} stdin: {e}"))?;
        return Ok(stdout);
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("git {args:?} failed: {e}"))?;
    finish_git(args, out)
}

fn finish_git(args: &[&str], out: std::process::Output) -> Result<Vec<u8>, String> {
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!(
            "git {args:?} exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn hash_text(root: &std::path::Path, text: &str) -> Result<String, String> {
    let out = git_output(root, &["hash-object", "--stdin"], Some(text.as_bytes()))?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

/// One `git hash-object --stdin-paths` over the paths that exist; a missing
/// path is `None` (the `absent` marker).
fn hash_paths(
    root: &std::path::Path,
    paths: &[String],
) -> Result<Vec<(String, Option<String>)>, String> {
    // A directory (e.g. an untracked symlink to one, which `ls-files` lists as a
    // file) cannot be hash-object'ed: it is `absent`, the same rule the Node
    // side applies. `is_dir` follows symlinks, like Node's `statSync`.
    let present: Vec<&String> = paths
        .iter()
        .filter(|p| {
            let q = root.join(p);
            q.exists() && !q.is_dir()
        })
        .collect();
    let mut oids: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
    if !present.is_empty() {
        let mut input = String::new();
        for p in &present {
            input.push_str(p);
            input.push('\n');
        }
        let out = git_output(
            root,
            &["hash-object", "--stdin-paths"],
            Some(input.as_bytes()),
        )?;
        let text = String::from_utf8_lossy(&out);
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() != present.len() {
            return Err(format!(
                "git hash-object returned {} oids for {} paths",
                lines.len(),
                present.len()
            ));
        }
        for (p, oid) in present.iter().zip(lines) {
            oids.insert(p.as_str(), oid.trim().to_string());
        }
    }
    Ok(paths
        .iter()
        .map(|p| (p.clone(), oids.get(p.as_str()).cloned()))
        .collect())
}

fn unignored_paths(root: &std::path::Path, pathspecs: &[&str]) -> Result<Vec<String>, String> {
    let mut args = vec![
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
        "--",
    ];
    args.extend_from_slice(pathspecs);
    let out = git_output(root, &args, None)?;
    let mut paths: Vec<String> = out
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).replace('\\', "/"))
        .collect();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn rust_src_hash(root: &std::path::Path) -> Result<String, String> {
    let inputs = unignored_paths(root, RUST_SRC_PATHSPECS)?;
    hash_text(root, &fold_lines(&hash_paths(root, &inputs)?))
}

/// Paths `git status --porcelain -z` reports, rename/copy sources included.
fn porcelain_paths(z: &[u8]) -> Vec<String> {
    let fields: Vec<&[u8]> = z.split(|b| *b == 0).filter(|s| !s.is_empty()).collect();
    let mut paths = Vec::new();
    let mut i = 0;
    while i < fields.len() {
        let f = fields[i];
        i += 1;
        if f.len() < 4 {
            continue;
        }
        let (x, y) = (f[0], f[1]);
        paths.push(String::from_utf8_lossy(&f[3..]).replace('\\', "/"));
        if matches!(x, b'R' | b'C') || matches!(y, b'R' | b'C') {
            if let Some(orig) = fields.get(i) {
                paths.push(String::from_utf8_lossy(orig).replace('\\', "/"));
                i += 1;
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

/// `(dirty, tree_hash)`. A clean tree's hash IS `HEAD^{tree}`, checkable with
/// one `git rev-parse`; a dirty tree folds `tree <HEAD^{tree}>` with every
/// changed path's current oid. `--no-optional-locks` keeps `status` from
/// taking the index lock a concurrent git command may hold. Nothing is written
/// to the object store (`hash-object` without `-w`).
fn tree_state(root: &std::path::Path) -> Result<(bool, String), String> {
    let head_tree =
        String::from_utf8_lossy(&git_output(root, &["rev-parse", "HEAD^{tree}"], None)?)
            .trim()
            .to_string();
    let status = git_output(
        root,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain",
            "-z",
            "--untracked-files=all",
        ],
        None,
    )?;
    let changed = porcelain_paths(&status);
    if changed.is_empty() {
        return Ok((false, head_tree));
    }
    let fold = format!(
        "tree {head_tree}\n{}",
        fold_lines(&hash_paths(root, &changed)?)
    );
    Ok((true, hash_text(root, &fold)?))
}

/// Whether the embedded dist's recorded frontend inputs still hash to the
/// `frontendSrcHash` recorded in `dist/provenance.json`.
#[derive(Debug, PartialEq)]
enum HalvesAgree {
    Yes,
    No {
        recomputed: String,
        differing: Vec<String>,
    },
    Unknown(String),
}

impl HalvesAgree {
    fn wire(&self) -> &'static str {
        match self {
            HalvesAgree::Yes => "true",
            HalvesAgree::No { .. } => "false",
            HalvesAgree::Unknown(_) => PROVENANCE_UNKNOWN,
        }
    }
}

/// `(recorded frontendSrcHash, recorded inputs)` out of `provenance.json`.
fn parse_provenance(json: &str) -> Result<(String, Vec<(String, Option<String>)>), String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("dist/provenance.json unparseable: {e}"))?;
    let recorded = v
        .get("frontendSrcHash")
        .and_then(|h| h.as_str())
        .filter(|h| !h.is_empty())
        .unwrap_or(PROVENANCE_UNKNOWN)
        .to_string();
    let inputs = v
        .get("inputs")
        .and_then(|i| i.as_array())
        .ok_or("dist/provenance.json has no inputs list")?
        .iter()
        .map(|e| {
            let path = e
                .get("path")
                .and_then(|p| p.as_str())
                .ok_or("an input has no path")?;
            let oid = e.get("oid").and_then(|o| o.as_str()).map(str::to_string);
            Ok((path.to_string(), oid))
        })
        .collect::<Result<Vec<_>, &str>>()?;
    Ok((recorded, inputs))
}

fn check_halves(
    root: &std::path::Path,
    recorded: &str,
    inputs: &[(String, Option<String>)],
) -> HalvesAgree {
    if recorded == PROVENANCE_UNKNOWN {
        return HalvesAgree::Unknown("the frontend build could not measure its inputs".into());
    }
    if inputs.is_empty() {
        // A fold over nothing would "match" any edit.
        return HalvesAgree::Unknown("dist/provenance.json records no inputs".into());
    }
    let paths: Vec<String> = inputs.iter().map(|(p, _)| p.clone()).collect();
    let now = match hash_paths(root, &paths) {
        Ok(n) => n,
        Err(e) => return HalvesAgree::Unknown(e),
    };
    let recomputed = match hash_text(root, &fold_lines(&now)) {
        Ok(h) => h,
        Err(e) => return HalvesAgree::Unknown(e),
    };
    if recomputed == recorded {
        return HalvesAgree::Yes;
    }
    let before: std::collections::HashMap<&str, &Option<String>> =
        inputs.iter().map(|(p, o)| (p.as_str(), o)).collect();
    let differing = now
        .iter()
        .filter(|(p, o)| before.get(p.as_str()).copied() != Some(o))
        .map(|(p, _)| p.clone())
        .collect();
    HalvesAgree::No {
        recomputed,
        differing,
    }
}

fn stamp_provenance() {
    // Manifest and lock only: an edit there rebuilds every target anyway, so
    // re-running this script costs nothing extra. `src` / `../src` are NOT
    // watched -- see the section comment for the cost and what keeps the
    // stamps exact without them.
    for p in ["Cargo.toml", "../Cargo.toml", "../Cargo.lock"] {
        println!("cargo:rerun-if-changed={p}");
    }
    // Set to a fresh value by `pnpm run build:exe` so a sanctioned build always
    // re-stamps (see the section comment).
    println!("cargo:rerun-if-env-changed=QONTINUI_PROVENANCE_NONCE");

    let root = git_output(
        std::path::Path::new("."),
        &["rev-parse", "--show-toplevel"],
        None,
    )
    .map(|o| std::path::PathBuf::from(String::from_utf8_lossy(&o).trim()));

    let (dirty, tree_hash, rust_hash) = match &root {
        Ok(root) => {
            let (dirty, tree) = match tree_state(root) {
                Ok((d, t)) => (d.to_string(), t),
                Err(_) => (
                    PROVENANCE_UNKNOWN.to_string(),
                    PROVENANCE_UNKNOWN.to_string(),
                ),
            };
            let rust = rust_src_hash(root).unwrap_or_else(|_| PROVENANCE_UNKNOWN.to_string());
            (dirty, tree, rust)
        }
        Err(_) => (
            PROVENANCE_UNKNOWN.to_string(),
            PROVENANCE_UNKNOWN.to_string(),
            PROVENANCE_UNKNOWN.to_string(),
        ),
    };
    println!("cargo:rustc-env=QONTINUI_GIT_DIRTY={dirty}");
    println!("cargo:rustc-env=QONTINUI_TREE_HASH={tree_hash}");
    println!("cargo:rustc-env=QONTINUI_RUST_SRC_HASH={rust_hash}");

    // The embedded dist's own record. Absent is the normal state of a bare
    // `cargo check` and of a dist built before Phase 1 — `unknown`, and a
    // warning only when a real (build-id-stamped) dist lacks it.
    let (frontend_hash, agree) = match std::fs::read_to_string("../dist/provenance.json") {
        Err(_) => {
            if std::path::Path::new("../dist/build-id.txt").exists() {
                println!(
                    "cargo:warning=dist/provenance.json is missing — this dist predates \
                     provenance stamping, so /health cannot say which frontend sources the \
                     binary embeds. Run `pnpm run build` (or `pnpm run build:exe`)."
                );
            }
            (
                PROVENANCE_UNKNOWN.to_string(),
                HalvesAgree::Unknown("dist/provenance.json absent".into()),
            )
        }
        Ok(json) => match (parse_provenance(&json), &root) {
            (Err(e), _) => (PROVENANCE_UNKNOWN.to_string(), HalvesAgree::Unknown(e)),
            (Ok((recorded, _)), Err(e)) => (recorded, HalvesAgree::Unknown(e.clone())),
            (Ok((recorded, inputs)), Ok(root)) => {
                let agree = check_halves(root, &recorded, &inputs);
                (recorded, agree)
            }
        },
    };
    match &agree {
        HalvesAgree::No {
            recomputed,
            differing,
        } => {
            let shown: Vec<&str> = differing.iter().take(5).map(String::as_str).collect();
            println!(
                "cargo:warning=the embedded dist is STALE: its frontend sources changed since \
                 `pnpm run build` (recorded frontendSrcHash {frontend_hash}, now {recomputed}; \
                 {} file(s) differ, e.g. {}). /health will report halvesAgree=false. Fix: \
                 `pnpm run build:exe`.",
                differing.len(),
                shown.join(", ")
            );
        }
        HalvesAgree::Unknown(why) if frontend_hash != PROVENANCE_UNKNOWN => {
            println!(
                "cargo:warning=could not verify the embedded dist against its frontend \
                 sources ({why}); /health will report halvesAgree=null."
            );
        }
        _ => {}
    }
    println!("cargo:rustc-env=QONTINUI_FRONTEND_SRC_HASH={frontend_hash}");
    println!("cargo:rustc-env=QONTINUI_HALVES_AGREE={}", agree.wire());
}

/// Unit tests for the build script's pure helpers.
///
/// These run under `cargo test --lib` because `src/wedge_diagnostics.rs` pulls
/// this file into the library's test binary with `#[path]`; see the comment on
/// `main` above for why that indirection exists.
#[cfg(test)]
mod tests {
    use super::{flags_carry_tokio_unstable, rustflags_carry_tokio_unstable};

    /// `RUSTFLAGS="--cfg tokio_unstable"` — two whitespace-separated tokens.
    #[test]
    fn the_two_field_spelling_is_accepted() {
        assert!(rustflags_carry_tokio_unstable(
            None,
            Some("--cfg tokio_unstable")
        ));
    }

    /// `RUSTFLAGS="--cfg=tokio_unstable"` — one token. rustc accepts it, and the
    /// old guard rejected it, telling the developer to set a flag they had set.
    #[test]
    fn the_joined_spelling_is_accepted() {
        assert!(rustflags_carry_tokio_unstable(
            None,
            Some("--cfg=tokio_unstable")
        ));
        // And the same spelling arriving through the encoded variable, which is
        // what a `.cargo/config.toml` `rustflags = ["--cfg=tokio_unstable"]`
        // produces.
        assert!(rustflags_carry_tokio_unstable(
            Some("--cfg=tokio_unstable"),
            None
        ));
    }

    /// The authoritative variable is `\x1f`-separated, never whitespace-split.
    #[test]
    fn the_encoded_variable_is_split_on_the_unit_separator() {
        assert!(rustflags_carry_tokio_unstable(
            Some("--cfg\u{1f}tokio_unstable"),
            None
        ));
        assert!(rustflags_carry_tokio_unstable(
            Some("-C\u{1f}target-cpu=native\u{1f}--cfg\u{1f}tokio_unstable"),
            None
        ));
        // A field is ONE flag: `"--cfg tokio_unstable"` as a single `\x1f`
        // field is not two flags, and rustc would reject it too.
        assert!(!rustflags_carry_tokio_unstable(
            Some("--cfg tokio_unstable"),
            None
        ));
    }

    /// Nothing set at all — the case the guard exists for.
    #[test]
    fn an_absent_flag_is_rejected() {
        assert!(!rustflags_carry_tokio_unstable(None, None));
        assert!(!rustflags_carry_tokio_unstable(Some(""), None));
        assert!(!rustflags_carry_tokio_unstable(
            Some("-C\u{1f}debuginfo=2"),
            Some("-C debuginfo=2")
        ));
    }

    /// **Cargo's precedence.** `CARGO_ENCODED_RUSTFLAGS` present — even empty —
    /// is the whole truth; a stale `RUSTFLAGS` inherited from the shell must not
    /// wave the guard through, because rustc will never see the cfg and the
    /// build would panic inside `ConsoleLayer::build` at startup instead.
    #[test]
    fn a_stale_rustflags_cannot_satisfy_the_guard_when_cargo_encoded_is_present() {
        assert!(!rustflags_carry_tokio_unstable(
            Some(""),
            Some("--cfg tokio_unstable")
        ));
        assert!(!rustflags_carry_tokio_unstable(
            Some("-C\u{1f}debuginfo=2"),
            Some("--cfg=tokio_unstable")
        ));
    }

    /// A bare `tokio_unstable` with no `--cfg` in front of it is a value, not a
    /// cfg, and must not satisfy the guard.
    #[test]
    fn a_bare_token_is_not_a_cfg() {
        assert!(!flags_carry_tokio_unstable(["tokio_unstable"]));
        assert!(!flags_carry_tokio_unstable([
            "--allow",
            "tokio_unstable",
            "-C",
            "opt-level=0"
        ]));
        // ...but the flag directly after `--cfg` is.
        assert!(flags_carry_tokio_unstable([
            "--allow",
            "unused",
            "--cfg",
            "tokio_unstable"
        ]));
    }

    /// Empty fields (a trailing separator, a double space) must not break the
    /// two-token pairing.
    #[test]
    fn empty_fields_do_not_break_the_pairing() {
        assert!(rustflags_carry_tokio_unstable(
            Some("--cfg\u{1f}\u{1f}tokio_unstable\u{1f}"),
            None
        ));
        assert!(rustflags_carry_tokio_unstable(
            None,
            Some("  --cfg   tokio_unstable  ")
        ));
    }
}

/// The provenance fold is a byte contract shared with
/// `scripts/frontend-provenance.mjs`; the fixture pins both sides.
#[cfg(test)]
mod provenance_tests {
    use super::{fold_lines, parse_provenance, porcelain_paths};

    #[test]
    fn fold_matches_the_shared_fixture() {
        let fx: serde_json::Value =
            serde_json::from_str(include_str!("../scripts/fixtures/provenance-fold.json")).unwrap();
        let entries: Vec<(String, Option<String>)> = fx["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["path"].as_str().unwrap().to_string(),
                    e["oid"].as_str().map(str::to_string),
                )
            })
            .collect();
        assert_eq!(fold_lines(&entries), fx["expected_fold"].as_str().unwrap());
    }

    #[test]
    fn porcelain_parsing_keeps_rename_sources() {
        let z = b" M src/a.rs\0R  src/new.rs\0src/old.rs\0?? untracked.txt\0";
        assert_eq!(
            porcelain_paths(z),
            vec!["src/a.rs", "src/new.rs", "src/old.rs", "untracked.txt"]
        );
    }

    #[test]
    fn provenance_json_round_trips_and_absent_oids_are_none() {
        let (h, inputs) = parse_provenance(
            r#"{"frontendSrcHash":"abc","inputs":[{"path":"a","oid":"1"},{"path":"b","oid":null}]}"#,
        )
        .unwrap();
        assert_eq!(h, "abc");
        assert_eq!(
            inputs,
            vec![
                ("a".to_string(), Some("1".to_string())),
                ("b".to_string(), None)
            ]
        );
        assert!(parse_provenance("{").is_err());
        // A missing or empty hash is UNKNOWN, never "an empty hash to compare".
        assert_eq!(
            parse_provenance(r#"{"frontendSrcHash":"","inputs":[]}"#)
                .unwrap()
                .0,
            "unknown"
        );
        assert!(parse_provenance(r#"{"frontendSrcHash":"h"}"#).is_err());
    }
}
