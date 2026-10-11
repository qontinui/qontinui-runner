//! Do Node's `process.hrtime` and the probe clock share a timebase? Measured
//! with a pipe round trip, NTP-style: the probe stamps a request, node answers
//! with `process.hrtime.bigint()`, the probe stamps the answer. On one timebase
//! node's reading lies inside [send, receive], so its offset from the midpoint
//! is at most half the round trip; the shortest round trip bounds it tightest.
//! Every wait is bounded and the child is always reaped.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::clock::now_ns;
use crate::doctor::{alignment, ClockFacts};

/// The answering side: one hrtime reading per request line.
const NODE_SCRIPT: &str = "const rl=require('readline').createInterface({input:process.stdin});\
rl.on('line',()=>process.stdout.write(process.hrtime.bigint().toString()+'\\n'));\
rl.on('close',()=>process.exit(0));";

/// Round trips discarded while node warms up (JIT, first pipe writes).
const WARMUP: usize = 5;

/// Measure with `node` (a command on PATH or a path). `rounds` includes the
/// warm-up; the whole check is bounded by `budget`.
pub fn node_round_trip(node: &str, rounds: usize, budget: Duration) -> ClockFacts {
    let started = Instant::now();
    let mut child = match Command::new(node)
        .arg("-e")
        .arg(NODE_SCRIPT)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return ClockFacts::NoNode(format!("`{node}` did not start: {e}")),
    };
    let result = exchange(&mut child, rounds, budget, started);
    reap(&mut child);
    result
}

fn reap(child: &mut Child) {
    drop(child.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                // Our own child, by handle: never a kill by image name.
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
}

fn exchange(child: &mut Child, rounds: usize, budget: Duration, started: Instant) -> ClockFacts {
    let Some(stdout) = child.stdout.take() else {
        return ClockFacts::Failed("node stdout was not captured".into());
    };
    let Some(stdin) = child.stdin.as_mut() else {
        return ClockFacts::Failed("node stdin was not captured".into());
    };
    let (tx, rx) = mpsc::channel::<(String, u64)>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {
                    let t = now_ns();
                    if tx.send((line.trim().to_string(), t)).is_err() {
                        return;
                    }
                }
            }
        }
    });
    let mut samples: Vec<(i64, u64)> = Vec::new();
    for k in 0..rounds {
        let left = budget.saturating_sub(started.elapsed());
        if left.is_zero() {
            break;
        }
        let t_send = now_ns();
        if let Err(e) = stdin.write_all(b"p\n").and_then(|_| stdin.flush()) {
            return ClockFacts::Failed(format!("writing to node: {e}"));
        }
        let (line, t_recv) = match rx.recv_timeout(left) {
            Ok(v) => v,
            Err(_) => {
                return ClockFacts::Failed(format!(
                    "node gave no answer to request {k} within the {} ms budget",
                    budget.as_millis()
                ))
            }
        };
        let Ok(t_node) = line.parse::<u64>() else {
            return ClockFacts::Failed(format!(
                "node answered something that is not an hrtime: {line:?}"
            ));
        };
        if k < WARMUP {
            continue;
        }
        let rtt = t_recv.saturating_sub(t_send);
        let mid = t_send / 2 + t_recv / 2;
        samples.push(((t_node as i128 - mid as i128) as i64, rtt));
    }
    best_sample(&samples)
}

/// The shortest round trip's offset decides.
pub fn best_sample(samples: &[(i64, u64)]) -> ClockFacts {
    match samples.iter().min_by_key(|(_, rtt)| *rtt) {
        Some(&(offset, rtt)) => ClockFacts::Measured(alignment(offset, rtt, samples.len() as u32)),
        None => ClockFacts::Failed("no round trip completed after warm-up".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shortest_round_trip_decides() {
        let facts = best_sample(&[
            (5_000_000, 9_000_000),
            (100_000, 400_000),
            (-3_000_000, 1_000_000),
        ]);
        match facts {
            ClockFacts::Measured(a) => {
                assert_eq!(a.offset_ns, 100_000);
                assert_eq!(a.rtt_ns, 400_000);
                assert!(a.aligned);
                assert_eq!(a.samples, 3);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(best_sample(&[]), ClockFacts::Failed(_)));
    }

    #[test]
    fn a_missing_node_is_reported_not_assumed() {
        let facts = node_round_trip(
            "termhost-probe-no-such-node-binary",
            10,
            Duration::from_secs(2),
        );
        assert!(matches!(facts, ClockFacts::NoNode(_)), "{facts:?}");
    }

    #[test]
    fn a_child_that_answers_garbage_fails_cleanly() {
        // cargo (which runs these tests, and names itself in $CARGO) is not an
        // hrtime server, so the exchange must fail — not hang — and reap it.
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let facts = node_round_trip(&cargo, 8, Duration::from_secs(3));
        assert!(
            matches!(facts, ClockFacts::Failed(_) | ClockFacts::NoNode(_)),
            "{facts:?}"
        );
    }

    #[test]
    fn node_shares_the_probe_timebase_where_node_exists() {
        // The real check `doctor` and `selftest` run. Both CI legs have node;
        // a machine without it reports NoNode, which is not this test's subject.
        match node_round_trip("node", 40, Duration::from_secs(10)) {
            ClockFacts::Measured(a) => assert!(
                a.aligned,
                "process.hrtime is not on the probe timebase: {a:?}"
            ),
            ClockFacts::NoNode(why) => {
                eprintln!("node not runnable here ({why}); alignment not exercised")
            }
            ClockFacts::Failed(why) => panic!("node ran but the round trip failed: {why}"),
        }
    }
}
