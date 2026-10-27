//! The cadence the runner's periodic git walkers share: run a cycle, then wait
//! one full interval measured from the moment it ENDED.
//!
//! The worktree census and the fleet tree publisher both used to tick on a
//! fixed `tokio::time::interval` with `MissedTickBehavior::Skip`. A cycle longer
//! than the interval — measured 2026-10-23 on spaceship: a census walk of 1,920
//! rows in 3,152 s against a 300 s tick — then restarted on the next tick
//! boundary, so the work ran back to back and its git spawn RATE was bounded
//! only by how fast processes start. Making each step cheaper would only have
//! made the cycle restart sooner. Pacing from the end makes the gap a full
//! interval whatever a cycle costs, so a cheaper cycle is a lower rate. Plan
//! `2026-10-24-runner-git-probes-spawn-40-processes-per-second-and-fetch-into-empty-dirs` D1.

use std::future::Future;
use std::time::Duration;

/// One unit of work [`run_paced`] repeats.
pub(crate) trait PacedCycle {
    fn run(&mut self) -> impl Future<Output = ()> + Send + '_;
}

/// Run `cycle`, sleep `interval`, repeat. The first cycle runs at once.
/// `max_cycles` bounds the loop for a test; production passes `None`.
/// Cancellation is the caller's task being dropped, at either await.
pub(crate) async fn run_paced<C: PacedCycle>(
    interval: Duration,
    max_cycles: Option<usize>,
    cycle: &mut C,
) {
    let mut ran = 0usize;
    loop {
        cycle.run().await;
        ran += 1;
        if max_cycles.is_some_and(|m| ran >= m) {
            return;
        }
        tokio::time::sleep(interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CountingCycle {
        starts: Vec<tokio::time::Instant>,
        work: Duration,
    }

    impl PacedCycle for CountingCycle {
        async fn run(&mut self) {
            self.starts.push(tokio::time::Instant::now());
            tokio::time::sleep(self.work).await;
        }
    }

    #[test]
    fn each_cycle_starts_a_full_interval_after_the_last_one_ended() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap();
        rt.block_on(async {
            // A cycle (10 min) far longer than the interval (5 min) — the
            // shape that made the old Skip tick run the census back to back.
            let mut c = CountingCycle {
                starts: Vec::new(),
                work: Duration::from_secs(600),
            };
            run_paced(Duration::from_secs(300), Some(3), &mut c).await;
            assert_eq!(c.starts.len(), 3);
            for w in c.starts.windows(2) {
                assert_eq!(
                    w[1] - w[0],
                    Duration::from_secs(900),
                    "start-to-start = the cycle's own 600 s + a full 300 s interval"
                );
            }
        });
    }
}
