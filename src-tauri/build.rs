fn main() {
    // Tell Cargo to re-run this build script (and re-embed the frontend) when
    // the dist directory changes.  Without this, incremental builds silently
    // serve a stale frontend bundle from the compile-time cache.
    println!("cargo:rerun-if-changed=../dist");
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

    // Re-run this script when HEAD moves. qontinui-runner's .git lives at
    // ../.git relative to src-tauri.
    //   - .git/HEAD fires on branch switch / detached-head jumps.
    //   - .git/refs/heads/ (directory) fires on any new commit to any local
    //     branch, since refs/heads/<branch> is the file git updates when
    //     advancing a branch ref. Without this a fresh commit on the
    //     currently-checked-out branch would keep the old QONTINUI_GIT_SHA
    //     embedded, defeating the purpose of this stamp.
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs/heads");

    tauri_build::build()
}
