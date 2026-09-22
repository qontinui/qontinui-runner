//! Host-derived parallelism sizing for one CI dispatch.
//!
//! **Why this is derived and not a constant.** The CI-node lane runs on a
//! machine the manifest author has never seen. A static `cargo_build_jobs = 1`
//! is safe on a laptop and wastes a 128 GiB workstation; a static `= 8` OOMs
//! the laptop. The GitHub Actions lane hit the second half of that in
//! production and answered it by deriving both caps from the host's own
//! `MemTotal` (`qontinui-coord/.github/actions/cap-cargo-jobs/action.yml`).
//! This module is that rule, ported.
//!
//! **The two budgets are sized SEPARATELY and deliberately differ.** The
//! incident behind the Actions rule killed the agent TWICE on the same job:
//! attempt 1 during `Compiling qontinui-coord`, attempt 2 during the TEST
//! phase — with `6543 tests run: 6543 passed` logged moments before the agent
//! died. Bounding only the build leaves half that failure mode open, which is
//! exactly the hole coord's `.qontinui/ci.toml` papers over today by smuggling
//! `--test-threads 4` through argv.
//!
//! - **3 GiB per cargo build token.** A token is mostly an LLVM codegen
//!   thread, which is much cheaper than a whole rustc process, so this is
//!   deliberately generous. It is a sizing judgement, not a measurement — the
//!   Actions action says so in the same words, and no peak-RSS profile of the
//!   ~577k-LOC coord crate was ever taken.
//! - **1 GiB per test process**, a looser and independent budget: test
//!   processes exec'ing the same binary SHARE its text/rodata through the page
//!   cache, so only anon memory is per-process.
//! - **A hard floor of 1 build job at or below 8 GiB**, overriding the ratio's
//!   answer of 2. That is a measurement, not a judgement: the qontinui-runner
//!   workspace was observed still OOM-killing the agent at `JOBS=2` on a 7 GB
//!   host even with a 12 GB swapfile.
//!
//! The decision is a pure function of [`HostCapacity`] so it is testable
//! everywhere; only [`probe`] touches the host.

/// What the host reports about itself. `mem_bytes` is `None` when memory could
/// not be read at all — a real case on locked-down hosts, and the one where
/// guessing is worst.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HostCapacity {
    pub mem_bytes: Option<u64>,
    /// Usable parallelism. Always at least 1.
    pub cpus: u32,
}

/// The derived caps for one dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HostSizing {
    pub cargo_build_jobs: u32,
    pub test_threads: u32,
}

/// Bytes of headroom assumed per concurrent cargo build token (3 GiB).
const BYTES_PER_BUILD_TOKEN: u64 = 3 * 1024 * 1024 * 1024;
/// Bytes of anon footprint assumed per test process (1 GiB).
const BYTES_PER_TEST_PROCESS: u64 = 1024 * 1024 * 1024;
/// At or below this total memory the build cap is pinned to 1 regardless of
/// the ratio (measured: `JOBS=2` still OOM-killed a 7 GB host).
const SMALL_HOST_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Floor when memory is unreadable. Conservative on purpose: an unknown host
/// is treated as a small one, because the failure mode of guessing high is an
/// OOM-killed dispatch and the failure mode of guessing low is a slow one.
const UNKNOWN_HOST_BUILD_JOBS: u32 = 1;
const UNKNOWN_HOST_TEST_THREADS: u32 = 2;

/// Derive both caps. Pure — this is the whole sizing rule, and it mirrors
/// `cap-cargo-jobs`' arithmetic operation-for-operation, including the order
/// in which the floors and the cpu cap are applied (the small-host build floor
/// is applied LAST so it overrides the cpu cap too).
pub(crate) fn derive(cap: HostCapacity) -> HostSizing {
    let cpus = cap.cpus.max(1);
    let Some(mem) = cap.mem_bytes.filter(|m| *m > 0) else {
        return HostSizing {
            cargo_build_jobs: UNKNOWN_HOST_BUILD_JOBS,
            test_threads: UNKNOWN_HOST_TEST_THREADS.min(cpus),
        };
    };

    let mut build_jobs = (mem / BYTES_PER_BUILD_TOKEN).clamp(1, u32::MAX as u64) as u32;
    build_jobs = build_jobs.max(1).min(cpus);
    if mem <= SMALL_HOST_BYTES {
        build_jobs = 1;
    }

    let mut test_threads = (mem / BYTES_PER_TEST_PROCESS).clamp(1, u32::MAX as u64) as u32;
    // Floor of 2 so the suite never goes fully serial, then capped by cpus —
    // so a genuine 1-cpu host still gets 1.
    test_threads = test_threads.max(2).min(cpus);

    HostSizing {
        cargo_build_jobs: build_jobs,
        test_threads,
    }
}

/// Cargo build tokens that make up ONE concurrent CI dispatch slot, in this
/// module's own units ([`BYTES_PER_BUILD_TOKEN`] each). A slot is therefore
/// 4 × 3 GiB = 12 GiB of build-token headroom.
const BUILD_TOKENS_PER_SLOT: u64 = 4;
/// Usable cpus that make up ONE concurrent CI dispatch slot. Paired with
/// [`BUILD_TOKENS_PER_SLOT`] so a slot carries one build token per core — the
/// same token↔core pairing [`derive`] applies when it caps `build_jobs` at
/// `cpus`.
const CPUS_PER_SLOT: u32 = 4;
/// What a host with an unreadable memory probe is offered: a single slot, for
/// the same reason [`UNKNOWN_HOST_BUILD_JOBS`] is 1.
const UNKNOWN_HOST_SUGGESTED_SLOTS: u32 = 1;

/// Which host term bounds [`suggested_concurrent_builds`]. Surfaced so the
/// settings panel can name WHY a value above the suggestion is risky.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LimitingTerm {
    Cores,
    Memory,
}

/// How many CI dispatches this host can run side by side — a SUGGESTION for
/// `ci_node.max_concurrent_builds` when the operator has not set one.
///
/// `derive` answers parallelism *within* one dispatch; this answers how many
/// dispatches, in the same units: one slot is [`BUILD_TOKENS_PER_SLOT`] build
/// tokens and [`CPUS_PER_SLOT`] cores, and the host gets as many slots as BOTH
/// terms allow, never fewer than 1. Unreadable memory yields 1 — guessing high
/// on an unknown host is the OOM, guessing low is merely slow.
///
/// Pure. Pair it with [`share`] at dispatch time: admitting N dispatches is only
/// safe if each one then sizes itself against a 1/N share of the host.
pub(crate) fn suggested_concurrent_builds(cap: HostCapacity) -> u32 {
    suggestion_with_limit(cap).0
}

/// [`suggested_concurrent_builds`] plus the term that bound it (`None` when
/// memory is unreadable and the answer is the conservative fallback).
pub(crate) fn suggestion_with_limit(cap: HostCapacity) -> (u32, Option<LimitingTerm>) {
    let Some(mem) = cap.mem_bytes.filter(|m| *m > 0) else {
        return (UNKNOWN_HOST_SUGGESTED_SLOTS, None);
    };
    let by_cores = cap.cpus.max(1) / CPUS_PER_SLOT;
    let by_mem = (mem / (BUILD_TOKENS_PER_SLOT * BYTES_PER_BUILD_TOKEN)).min(u32::MAX as u64) as u32;
    let limit = if by_cores <= by_mem {
        LimitingTerm::Cores
    } else {
        LimitingTerm::Memory
    };
    (by_cores.min(by_mem).max(1), Some(limit))
}

/// One dispatch's share of the host when `n` dispatches run concurrently.
///
/// **Why this exists.** [`derive`] sizes one dispatch against whatever capacity
/// it is handed. Handed the WHOLE host while `n` dispatches run side by side,
/// each takes all of it and the node oversubscribes `n`-fold — `n × cpus` cargo
/// jobs and `n ×` the build-token memory, which is the OOM class this module
/// exists to prevent. So the executor derives from `share(probe(), n)`, where
/// `n` is the capacity the node admits against. `n` is clamped to at least 1,
/// and a share never drops below 1 cpu; `derive`'s own floors then keep a thin
/// share at 1 build job. Unreadable memory stays unreadable.
pub(crate) fn share(cap: HostCapacity, n: u32) -> HostCapacity {
    let n = n.max(1);
    HostCapacity {
        mem_bytes: cap.mem_bytes.map(|m| m / n as u64),
        cpus: (cap.cpus / n).max(1),
    }
}

/// Read the host's capacity. Blocking (a sysinfo memory refresh), so callers
/// run it once per dispatch rather than per step.
///
/// `available_parallelism` rather than a raw core count: it honours cgroup
/// quotas and processor affinity, which is what actually bounds a build in a
/// container or on a pinned WSL VM.
pub(crate) fn probe() -> HostCapacity {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get().min(u32::MAX as usize) as u32)
        .unwrap_or(1);
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let total = sys.total_memory();
    HostCapacity {
        mem_bytes: (total > 0).then_some(total),
        cpus,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn sizing(gib: u64, cpus: u32) -> HostSizing {
        derive(HostCapacity {
            mem_bytes: Some(gib * GIB),
            cpus,
        })
    }

    /// The failure mode this rule exists to prevent: a small host must not be
    /// handed its cpu count. 7 GiB / 8 vCPU is the shape that OOM-killed the
    /// Actions agent at JOBS=2.
    #[test]
    fn small_host_is_pinned_to_one_build_job() {
        for gib in [1, 2, 4, 7, 8] {
            let s = sizing(gib, 8);
            assert_eq!(s.cargo_build_jobs, 1, "{gib} GiB must get 1 build job");
        }
    }

    /// The other side of the constraint: a large host must not be wasted.
    #[test]
    fn large_host_gets_real_parallelism() {
        // 23 GiB / 32 vCPU — the measured spaceship-wsl shape from the
        // incident. 23/3 = 7 build tokens, 23/1 = 23 test processes.
        let s = sizing(23, 32);
        assert_eq!(s.cargo_build_jobs, 7);
        assert_eq!(s.test_threads, 23);

        // 128 GiB / 32 vCPU — memory is no longer the binding constraint, so
        // both caps land on the cpu count rather than the ratio's 42/128.
        let s = sizing(128, 32);
        assert_eq!(s.cargo_build_jobs, 32);
        assert_eq!(s.test_threads, 32);
    }

    /// The cpu cap binds before the memory ratio on a RAM-heavy, core-poor
    /// host; more jobs than cores buys nothing.
    #[test]
    fn cpu_count_caps_both_budgets() {
        let s = sizing(64, 2);
        assert_eq!(s.cargo_build_jobs, 2);
        assert_eq!(s.test_threads, 2);
    }

    /// The small-host build floor is applied AFTER the cpu cap, so it wins.
    #[test]
    fn small_host_floor_overrides_the_cpu_cap() {
        let s = sizing(8, 64);
        assert_eq!(s.cargo_build_jobs, 1);
        // Tests keep their own, looser budget: 8 GiB buys 8 processes.
        assert_eq!(s.test_threads, 8);
    }

    /// A single-cpu host gets 1 test thread — the floor of 2 must not beat
    /// the cpu cap.
    #[test]
    fn one_cpu_host_gets_one_of_each() {
        let s = sizing(64, 1);
        assert_eq!(s.cargo_build_jobs, 1);
        assert_eq!(s.test_threads, 1);
    }

    /// Unreadable memory is treated as a small host, never as an unbounded
    /// one.
    #[test]
    fn unknown_memory_falls_back_conservatively() {
        let s = derive(HostCapacity {
            mem_bytes: None,
            cpus: 64,
        });
        assert_eq!(s.cargo_build_jobs, 1);
        assert_eq!(s.test_threads, 2);

        // A zero reading is "unreadable", not "zero memory".
        let s = derive(HostCapacity {
            mem_bytes: Some(0),
            cpus: 64,
        });
        assert_eq!(s.cargo_build_jobs, 1);
        assert_eq!(s.test_threads, 2);
    }

    /// A degenerate cpu report never yields a zero cap (which would export
    /// `CARGO_BUILD_JOBS=0` and wedge cargo).
    #[test]
    fn zero_cpus_is_clamped_to_one() {
        let s = derive(HostCapacity {
            mem_bytes: Some(64 * GIB),
            cpus: 0,
        });
        assert_eq!(s.cargo_build_jobs, 1);
        assert_eq!(s.test_threads, 1);
    }

    fn cap(gib: u64, cpus: u32) -> HostCapacity {
        HostCapacity {
            mem_bytes: Some(gib * GIB),
            cpus,
        }
    }

    /// The five devices measured on 2026-09-22 (`GET /coord/fleet`), GB read
    /// as GiB. Plan `2026-09-22-ci-capacity-is-hand-typed-...`, Phase 2.
    #[test]
    fn suggestion_matches_the_measured_fleet() {
        for (cpus, gib, want, name) in [
            (48, 368, 12, "merytshost"),
            (32, 125, 8, "spaceship"),
            (16, 31, 2, "MSI"),
            (8, 31, 2, "monster"),
            (8, 15, 1, "nomad"),
        ] {
            assert_eq!(
                suggested_concurrent_builds(cap(gib, cpus)),
                want,
                "{name}: {cpus}c/{gib}GiB"
            );
        }
    }

    /// The suggestion never drops to 0 — a tiny or unreadable host gets 1.
    #[test]
    fn suggestion_is_never_zero() {
        assert_eq!(suggested_concurrent_builds(cap(1, 1)), 1);
        assert_eq!(suggested_concurrent_builds(cap(1, 0)), 1);
        for mem_bytes in [None, Some(0)] {
            assert_eq!(
                suggested_concurrent_builds(HostCapacity {
                    mem_bytes,
                    cpus: 64
                }),
                1
            );
        }
    }

    /// The limiting term names the binding constraint.
    #[test]
    fn suggestion_names_its_limiting_term() {
        assert_eq!(
            suggestion_with_limit(cap(368, 48)),
            (12, Some(LimitingTerm::Cores))
        );
        assert_eq!(
            suggestion_with_limit(cap(31, 16)),
            (2, Some(LimitingTerm::Memory))
        );
        assert_eq!(
            suggestion_with_limit(HostCapacity {
                mem_bytes: None,
                cpus: 8
            }),
            (1, None)
        );
    }

    /// Per-dispatch partitioning: 12 dispatches on merytshost each size
    /// against a 1/12 share, not the whole host.
    #[test]
    fn a_share_sizes_one_of_n_dispatches() {
        let host = cap(368, 48);
        let s = derive(share(host, 12));
        // share = 30.67 GiB / 4 cpus → build min(10, 4) = 4, tests min(30, 4) = 4.
        assert_eq!(s.cargo_build_jobs, 4);
        assert_eq!(s.test_threads, 4);
        // Unpartitioned, the same host hands EACH dispatch 48 of each.
        assert_eq!(derive(host).cargo_build_jobs, 48);
    }

    /// N = 1 (and a degenerate N = 0) is exactly the old whole-host derive.
    #[test]
    fn a_share_of_one_is_the_whole_host() {
        for host in [
            cap(368, 48),
            cap(7, 8),
            HostCapacity {
                mem_bytes: None,
                cpus: 4,
            },
        ] {
            assert_eq!(share(host, 1), host);
            assert_eq!(share(host, 0), host);
            assert_eq!(derive(share(host, 1)), derive(host));
        }
    }

    /// A share never reports zero cpus, even when N exceeds the core count.
    #[test]
    fn a_share_keeps_at_least_one_cpu() {
        let s = share(cap(64, 4), 12);
        assert_eq!(s.cpus, 1);
        let d = derive(s);
        assert_eq!(d.cargo_build_jobs, 1);
        assert_eq!(d.test_threads, 1);
    }

    /// The probe is IO, so this only asserts the invariant every caller
    /// depends on: the derived caps are never zero.
    #[test]
    fn probe_yields_usable_caps_on_this_host() {
        let s = derive(probe());
        assert!(s.cargo_build_jobs >= 1);
        assert!(s.test_threads >= 1);
    }
}
